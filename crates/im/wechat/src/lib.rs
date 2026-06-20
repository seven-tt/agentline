//! WeChat iLink bot adapter for agentline.
//!
//! Talks to https://ilinkai.weixin.qq.com — Tencent's officially-released
//! personal-WeChat Bot HTTP API (the iLink protocol).

rust_i18n::i18n!("../core/locales", fallback = "zh-CN");

pub mod auth;
pub mod error;
pub mod http;
pub mod markdown;
pub mod media;
pub mod poll;
pub mod send;
pub mod stream;
pub mod token;
pub mod types;

/// Channel version reported to iLink in every `base_info`.
pub const CHANNEL_VERSION: &str = "1.0.2";

pub use auth::{LoginResult, QrCode, request_qr, wait_for_scan};
pub use error::{Error, Result};
pub use http::HttpClient;
pub use poll::{ContextTokenCache, CursorCell, CursorPersist, NoopPersist, spawn_poller};
pub use token::TokenRegistry;

use agentline_im_core::event::OutboundEvent;
use agentline_im_core::source::{ImAdapter, ImCapabilities, InputSource, InputSourceKind};
use agentline_im_core::types::{
    AgentUpdate, ElicitationPropertySchema, PeerRef, multi_select_options, single_select_options,
};
use agentline_im_core::{AgentEvent, PermissionDanger, RenderState, synthesize};
use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::sync::RwLock;

/// Minimum gap between consecutive outbound sends. iLink returns `ret=-2`
/// when messages are sent too rapidly (independent of message length), so the
/// [`SendQueue`] worker dispatches at most one queued message per this interval.
const MIN_SEND_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1000);

/// Idle timeout before an active stream is force-closed.
const STREAM_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// How long a blocked sender waits between re-checks for a fresh token.
const TOKEN_WAIT_POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// Appended to the message that spends a user's last send slot, prompting them
/// to top up the budget by sending another message.
fn continue_hint() -> String {
    rust_i18n::t!("im.wechat_continue_hint").to_string()
}

/// Queued, token-bucket-paced sender for discrete text messages.
///
/// Outbound messages are pushed onto an unbounded channel; a single background
/// worker drains them. Each send must claim a slot from the [`TokenRegistry`]:
/// every inbound user message grants a budget of [`token::MAX_MSGS_PER_CONTEXT`]
/// sends on its `context_token`, and iLink rejects anything beyond that with
/// `ret=-2`. The worker spends tokens oldest-first; when the budget runs out it
/// appends a continue hint to the final message and **blocks** (messages stay
/// queued) until the user sends again, which tops the bucket back up. A modest
/// [`MIN_SEND_INTERVAL`] gap is still kept between sends. The worker exits when
/// all `SendQueue` clones are dropped.
///
/// Intra-stream piece sends (`send_piece` / `send_stream_signal`) deliberately
/// bypass this queue: they belong to an already-open stream.
#[derive(Clone)]
struct SendQueue {
    tx: tokio::sync::mpsc::UnboundedSender<(PeerRef, String)>,
}

impl SendQueue {
    /// Spawn the background worker and return a handle for enqueuing.
    fn spawn(
        http: HttpClient,
        interval: std::time::Duration,
        registry: TokenRegistry,
        agent_done: Arc<Mutex<HashSet<String>>>,
    ) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(PeerRef, String)>();
        tokio::spawn(async move {
            let mut last: Option<std::time::Instant> = None;
            while let Some((peer, text)) = rx.recv().await {
                Self::deliver(
                    &http,
                    &registry,
                    &agent_done,
                    &mut last,
                    interval,
                    peer,
                    text,
                )
                .await;
            }
            tracing::debug!("send queue worker stopped");
        });
        Self { tx }
    }

    /// Claim a token and send `text`, blocking until a slot is available.
    /// Drops the message only if the bucket is empty *and* the newest token has
    /// aged out (the user is unlikely to ever top it up).
    async fn deliver(
        http: &HttpClient,
        registry: &TokenRegistry,
        agent_done: &Mutex<HashSet<String>>,
        last: &mut Option<std::time::Instant>,
        interval: std::time::Duration,
        peer: PeerRef,
        text: String,
    ) {
        let user = peer.user_id.clone();
        loop {
            match registry.claim(&user) {
                token::Slot::Grant {
                    token,
                    last: final_slot,
                } => {
                    let target = with_context_token(peer.clone(), &token);
                    let done = agent_done.lock().await.contains(&user);
                    let body = if final_slot && !done {
                        format!("{text}{}", continue_hint())
                    } else {
                        text.clone()
                    };
                    if let Some(prev) = *last {
                        let elapsed = prev.elapsed();
                        if elapsed < interval {
                            tokio::time::sleep(interval - elapsed).await;
                        }
                    }
                    let result = send::send_text(http, &target, &body).await;
                    *last = Some(std::time::Instant::now());
                    match result {
                        Ok(()) => return,
                        // `ret=-2` = this token's reply window is closed on the
                        // server. Mark it dead so we stop hammering it, then loop
                        // to block until the user tops up with a new message —
                        // the same message is retried on the fresh token.
                        Err(crate::error::Error::Api { ret: -2, .. }) => {
                            tracing::warn!(user=%user, "ret=-2; token closed, waiting for a new one");
                            registry.exhaust(&user, &token);
                        }
                        Err(e) => {
                            tracing::error!(error=%e, "queued send failed");
                            return;
                        }
                    }
                }
                token::Slot::Exhausted { stale: true } => {
                    tracing::warn!(
                        user = %user,
                        "send budget spent and token aged out; dropping message"
                    );
                    return;
                }
                token::Slot::Exhausted { stale: false } | token::Slot::Unknown => {
                    // Wait for the user to send again (tops up the bucket).
                    registry.wait(TOKEN_WAIT_POLL).await;
                }
            }
        }
    }

    /// Push a message onto the queue. Returns immediately; the worker sends it
    /// once a token slot is available.
    fn enqueue(&self, peer: PeerRef, text: String) {
        if self.tx.send((peer, text)).is_err() {
            tracing::error!("send queue closed; dropping outbound message");
        }
    }
}

/// Return `peer` with its `opaque.context_token` overridden to `token`, so the
/// send uses the slot the worker just claimed (which may differ from the token
/// captured when the message was enqueued).
fn with_context_token(mut peer: PeerRef, token: &str) -> PeerRef {
    peer.opaque["context_token"] = serde_json::Value::String(token.to_string());
    peer
}

/// Max bytes per piece payload (16 KiB).
const MAX_PIECE_BYTES: usize = 16 * 1024;

/// Max bytes for a single plain-text message. WeChat truncates messages beyond
/// ~2048 bytes; we use a slightly conservative limit and split at paragraph
/// boundaries to avoid mid-sentence cuts.
const MAX_PLAIN_BYTES: usize = 1800;

/// A per-peer output session.
///
/// When the iLink piece-streaming API is available we open a `WeixinStreamSender`
/// and push incremental pieces.  When it is unavailable (e.g. `native_init_stream`
/// 404s) we fall back to buffering filtered text and sending it in one go on
/// `StreamEnd` so the user does not see fragmented message bubbles.
struct ActiveStream {
    sender: Option<stream::WeixinStreamSender>,
    md_filter: markdown::StreamingMarkdownFilter,
    last_activity: std::time::Instant,
    /// True once the stream_start signal message has been sent.
    signaled: bool,
    /// Accumulated text when streaming is not available.
    buffered_text: String,
    /// Byte length of the `"🤖 {tag} "` header prefix within `buffered_text`,
    /// so a tool call that interrupts the stream before any chunk arrives
    /// can be detected and the header-only message suppressed.
    #[allow(dead_code)]
    header_len: usize,
    /// Accumulated thinking char count for summary on ThinkingEnd.
    thinking_chars: usize,
}

/// Buffered tool messages waiting to be merged into a single markdown send.
/// When the agent fires multiple tool calls in quick succession, we collect
/// their completion lines and flush them as one message — either when 5 seconds
/// elapse with no new tool event, or when a non-tool event arrives.
struct ToolBatch {
    lines: Vec<String>,
    /// Tool stdout/stderr accumulated from ToolProgress events, keyed by tool id.
    progress: HashMap<String, String>,
    /// Inter-tool assistant text accumulated while the batch is active.
    /// Committed as a line when the next ToolEnd arrives or the batch flushes.
    stream_buf: String,
    peer: Option<PeerRef>,
    flush_handle: Option<tokio::task::JoinHandle<()>>,
}

/// Maximum time to wait for more tool events before flushing the batch.
const TOOL_BATCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Maximum chars of inter-tool assistant text to absorb before flushing the
/// batch and switching to normal streaming. Keeps short comments merged with
/// tools while preventing an entire long response from being buffered.
const BATCH_STREAM_ABSORB_MAX: usize = 1000;

/// Parameters for deferred start via `InputSource::start()`.
struct DeferredStart {
    initial_cursor: String,
    persist: Arc<dyn CursorPersist>,
    allowed_users: Vec<String>,
}

/// iLink adapter. Construct via `WechatChannel::start` which also
/// kicks off the long-poll loop and returns the inbound mpsc receiver.
pub struct WechatChannel {
    http: HttpClient,
    /// Rate-limited, queued sender for discrete text messages.
    send_queue: SendQueue,
    /// Active streams per peer.
    active_streams: Arc<Mutex<HashMap<String, ActiveStream>>>,
    /// Stable device id for piece streaming.
    device_id: String,
    /// Per-user `context_token` budgets. Seeded from persisted state on startup,
    /// topped up on every inbound message, and spent by the send queue. Also
    /// the fallback source of a token when a `PeerRef`'s opaque field lacks one.
    registry: TokenRegistry,
    /// Batched tool completion messages (merged into one send after a quiet period).
    tool_batch: Arc<Mutex<ToolBatch>>,
    /// Users whose agent turn has completed. Shared with the poller so that a
    /// "继续" received after the agent is done can be answered instead of silently
    /// swallowed.
    agent_done: Arc<Mutex<HashSet<String>>>,
    /// Per-user typing indicator loop handles.
    typing_guards: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    /// Per-session render synthesis state, keyed by session id.
    render_states: Arc<Mutex<HashMap<String, RenderState>>>,
    /// Deferred start config (consumed by `InputSource::start()`).
    deferred_start: Mutex<Option<DeferredStart>>,
}

impl WechatChannel {
    /// Returns (channel, inbound_rx, poll_join_handle, cursor_cell).
    /// The caller owns the join handle and the cursor cell (for persistence).
    ///
    /// `initial_context_tokens` should be loaded from the persisted state so
    /// that known users can be reached immediately after a restart.
    pub fn start(
        http: HttpClient,
        initial_cursor: String,
        persist: Arc<dyn CursorPersist>,
        allowed_users: Vec<String>,
        initial_context_tokens: HashMap<String, String>,
    ) -> (
        Self,
        tokio::sync::mpsc::Receiver<agentline_im_core::types::SourceMessage>,
        tokio::task::JoinHandle<()>,
        CursorCell,
    ) {
        let cursor: CursorCell = Arc::new(RwLock::new(initial_cursor));
        let registry = TokenRegistry::new(token::MAX_MSGS_PER_CONTEXT, token::TOKEN_MAX_AGE);
        registry.seed(initial_context_tokens);
        let agent_done: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        let send_queue = SendQueue::spawn(
            http.clone(),
            MIN_SEND_INTERVAL,
            registry.clone(),
            agent_done.clone(),
        );
        let (rx, handle) = poll::spawn_poller(
            http.clone(),
            cursor.clone(),
            persist,
            Arc::new(allowed_users),
            registry.clone(),
            agent_done.clone(),
            32,
        );

        // Background GC task: drop spent / aged-out tokens (keeps each user's
        // newest so they remain reachable).
        let gc_registry = registry.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                iv.tick().await;
                gc_registry.gc();
            }
        });
        let active_streams: Arc<Mutex<HashMap<String, ActiveStream>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Background GC task: close idle streams.
        let gc_streams = active_streams.clone();
        let gc_http = http.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                interval.tick().await;
                let mut streams = gc_streams.lock().await;
                let now = std::time::Instant::now();
                let stale: Vec<String> = streams
                    .iter()
                    .filter(|(_, s)| now.duration_since(s.last_activity) > STREAM_IDLE_TIMEOUT)
                    .map(|(k, _)| k.clone())
                    .collect();
                for peer_id in stale {
                    if let Some(mut active) = streams.remove(&peer_id) {
                        flush_and_end(&gc_http, &mut active).await;
                        tracing::debug!(peer_id, "closed idle stream");
                    }
                }
            }
        });

        (
            Self {
                send_queue,
                http,
                active_streams,
                device_id: format!("agentline-{}", rand::random::<u32>()),
                registry,
                tool_batch: Arc::new(Mutex::new(ToolBatch {
                    lines: Vec::new(),
                    progress: HashMap::new(),
                    stream_buf: String::new(),
                    peer: None,
                    flush_handle: None,
                })),
                agent_done,
                typing_guards: Arc::new(Mutex::new(HashMap::new())),
                render_states: Arc::new(Mutex::new(HashMap::new())),
                deferred_start: Mutex::new(None),
            },
            rx,
            handle,
            cursor,
        )
    }

    /// Create a WechatChannel that can be started later via `InputSource::start()`.
    pub fn new(
        http: HttpClient,
        initial_cursor: String,
        persist: Arc<dyn CursorPersist>,
        allowed_users: Vec<String>,
        initial_context_tokens: HashMap<String, String>,
    ) -> Self {
        let registry = TokenRegistry::new(token::MAX_MSGS_PER_CONTEXT, token::TOKEN_MAX_AGE);
        registry.seed(initial_context_tokens);
        let agent_done: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        let send_queue = SendQueue::spawn(
            http.clone(),
            MIN_SEND_INTERVAL,
            registry.clone(),
            agent_done.clone(),
        );
        let active_streams: Arc<Mutex<HashMap<String, ActiveStream>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Background GC task: drop spent / aged-out tokens.
        let gc_registry = registry.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                iv.tick().await;
                gc_registry.gc();
            }
        });

        // Background GC task: close idle streams.
        let gc_streams = active_streams.clone();
        let gc_http = http.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                interval.tick().await;
                let mut streams = gc_streams.lock().await;
                let now = std::time::Instant::now();
                let stale: Vec<String> = streams
                    .iter()
                    .filter(|(_, s)| now.duration_since(s.last_activity) > STREAM_IDLE_TIMEOUT)
                    .map(|(k, _)| k.clone())
                    .collect();
                for peer_id in stale {
                    if let Some(mut active) = streams.remove(&peer_id) {
                        flush_and_end(&gc_http, &mut active).await;
                        tracing::debug!(peer_id, "closed idle stream");
                    }
                }
            }
        });

        Self {
            send_queue,
            http,
            active_streams,
            device_id: format!("agentline-{}", rand::random::<u32>()),
            registry,
            tool_batch: Arc::new(Mutex::new(ToolBatch {
                lines: Vec::new(),
                progress: HashMap::new(),
                stream_buf: String::new(),
                peer: None,
                flush_handle: None,
            })),
            agent_done,
            typing_guards: Arc::new(Mutex::new(HashMap::new())),
            render_states: Arc::new(Mutex::new(HashMap::new())),
            deferred_start: Mutex::new(Some(DeferredStart {
                initial_cursor,
                persist,
                allowed_users,
            })),
        }
    }

    pub fn http(&self) -> &HttpClient {
        &self.http
    }

    /// If `peer.opaque` already has a `context_token`, return a clone of peer.
    /// Otherwise inject the user's newest known token from the registry. (The
    /// send queue re-selects the token per message; this only matters for
    /// non-queued paths such as the typing indicator.)
    async fn enrich_peer(&self, peer: &PeerRef) -> PeerRef {
        if peer
            .opaque
            .get("context_token")
            .and_then(|v| v.as_str())
            .is_some()
        {
            return peer.clone();
        }
        if let Some(token) = self.registry.latest(&peer.user_id) {
            return with_context_token(peer.clone(), &token);
        }
        peer.clone()
    }

    /// End and remove any active stream for the given peer, returning any
    /// buffered text that still needs to be sent.
    async fn end_stream_for(&self, peer_id: &str) -> Option<String> {
        let mut streams = self.active_streams.lock().await;
        if let Some(mut active) = streams.remove(peer_id) {
            let buffered = flush_and_end(&self.http, &mut active).await;
            if !buffered.is_empty() {
                return Some(buffered);
            }
        }
        None
    }

    /// Enqueue a plain text message for rate-limited delivery.
    ///
    /// Returns as soon as the message is queued; the [`SendQueue`] worker
    /// performs the actual send no faster than one per [`MIN_SEND_INTERVAL`].
    /// Empty messages are dropped.
    async fn send_plain(&self, to: &PeerRef, text: &str) -> agentline_im_core::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let to = self.enrich_peer(to).await;
        if text.len() <= MAX_PLAIN_BYTES {
            self.send_queue.enqueue(to, text.to_string());
        } else {
            for chunk in split_plain_message(text, MAX_PLAIN_BYTES) {
                self.send_queue.enqueue(to.clone(), chunk);
            }
        }
        Ok(())
    }

    /// Send all buffered tool lines as a single merged message.
    /// The `stream_buf` (inter-tool assistant text) is sent as a separate
    /// message so the final response is never truncated by merging with tools.
    async fn flush_tool_batch(&self) {
        let (lines, stream_buf, peer) = {
            let mut batch = self.tool_batch.lock().await;
            let buf = std::mem::take(&mut batch.stream_buf);
            if batch.lines.is_empty() && buf.is_empty() {
                batch.progress.clear();
                return;
            }
            if let Some(h) = batch.flush_handle.take() {
                h.abort();
            }
            batch.progress.clear();
            (std::mem::take(&mut batch.lines), buf, batch.peer.take())
        };
        if let Some(peer) = peer {
            let mut parts: Vec<String> = Vec::new();
            if !lines.is_empty() {
                parts.push(lines.join("\n\n"));
            }
            if !stream_buf.is_empty() {
                parts.push(stream_buf);
            }
            if !parts.is_empty() {
                let text = parts.join("\n\n");
                if let Err(e) = self.send_plain(&peer, &text).await {
                    tracing::error!(error=%e, "flush_tool_batch send failed");
                }
            }
        }
    }

    /// Push a tool completion line into the batch and (re)start the flush timer.
    async fn push_tool_line(&self, peer: &PeerRef, line: String) {
        let batch_ref = self.tool_batch.clone();
        let mut batch = self.tool_batch.lock().await;
        // Commit any inter-tool text accumulated before this tool completion.
        if !batch.stream_buf.is_empty() {
            let buf = std::mem::take(&mut batch.stream_buf);
            batch.lines.push(buf);
        }
        batch.lines.push(line);
        batch.peer = Some(peer.clone());
        Self::reset_batch_timer(
            &mut batch,
            batch_ref,
            self.send_queue.clone(),
            self.registry.clone(),
        );
    }

    /// Ensure a typing indicator loop is running for the given peer. If one is
    /// already active this is a no-op.
    async fn ensure_typing(&self, peer: &PeerRef) {
        let user_id = peer.user_id.clone();
        let mut guards = self.typing_guards.lock().await;
        if let Some(h) = guards.get(&user_id)
            && !h.is_finished()
        {
            return;
        }
        let http = self.http.clone();
        let registry = self.registry.clone();
        let peer = peer.clone();
        let interval = self.typing_interval();
        let handle = tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            loop {
                tick.tick().await;
                if registry.is_exhausted(&peer.user_id) {
                    continue;
                }
                let enriched = if peer
                    .opaque
                    .get("context_token")
                    .and_then(|v| v.as_str())
                    .is_some()
                {
                    peer.clone()
                } else if let Some(tok) = registry.latest(&peer.user_id) {
                    with_context_token(peer.clone(), &tok)
                } else {
                    continue;
                };
                let _ = send::send_typing(&http, &enriched, 1).await;
            }
        });
        guards.insert(user_id, handle);
    }

    /// Cancel the typing indicator loop for the given peer and send status=2
    /// to clear the indicator on the client side.
    async fn cancel_typing(&self, peer: &PeerRef) {
        let mut guards = self.typing_guards.lock().await;
        if let Some(h) = guards.remove(&peer.user_id) {
            h.abort();
            let enriched = self.enrich_peer(peer).await;
            let _ = send::send_typing(&self.http, &enriched, 2).await;
        }
    }

    /// (Re)start the flush timer on the batch. Shared by push_tool_line and
    /// absorb_into_batch.
    fn reset_batch_timer(
        batch: &mut ToolBatch,
        batch_ref: Arc<Mutex<ToolBatch>>,
        send_queue: SendQueue,
        registry: TokenRegistry,
    ) {
        if let Some(h) = batch.flush_handle.take() {
            h.abort();
        }
        batch.flush_handle = Some(tokio::spawn(async move {
            tokio::time::sleep(TOOL_BATCH_TIMEOUT).await;
            let (lines, stream_buf, peer) = {
                let mut b = batch_ref.lock().await;
                let buf = std::mem::take(&mut b.stream_buf);
                if b.lines.is_empty() && buf.is_empty() {
                    b.progress.clear();
                    return;
                }
                b.progress.clear();
                (std::mem::take(&mut b.lines), buf, b.peer.take())
            };
            if let Some(peer) = peer {
                let enriched = if peer
                    .opaque
                    .get("context_token")
                    .and_then(|v| v.as_str())
                    .is_some()
                {
                    peer
                } else if let Some(tok) = registry.latest(&peer.user_id) {
                    let mut p = peer;
                    p.opaque["context_token"] = serde_json::Value::String(tok);
                    p
                } else {
                    peer
                };
                let mut parts: Vec<String> = Vec::new();
                if !lines.is_empty() {
                    parts.push(lines.join("\n\n"));
                }
                if !stream_buf.is_empty() {
                    parts.push(stream_buf);
                }
                if !parts.is_empty() {
                    let text = parts.join("\n\n");
                    for chunk in split_plain_message(&text, MAX_PLAIN_BYTES) {
                        send_queue.enqueue(enriched.clone(), chunk);
                    }
                }
            }
        }));
    }
}

/// Flush the markdown filter and, depending on the stream mode, either end the
/// piece stream or collect the remaining text into `buffered_text`.
/// Returns the full text that should be sent as a fallback plain message.
async fn flush_and_end(_http: &HttpClient, active: &mut ActiveStream) -> String {
    let remaining = active.md_filter.flush();
    if !remaining.is_empty() {
        active.buffered_text.push_str(&remaining);
    }

    if let Some(ref mut sender) = active.sender
        && !sender.is_ended()
    {
        let _ = sender.end().await;
    }

    std::mem::take(&mut active.buffered_text)
}

/// Split a long plain message into multiple parts at paragraph boundaries
/// (double newline), falling back to single newline, then hard byte boundary.
fn split_plain_message(text: &str, max_bytes: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut remaining = text;
    while remaining.len() > max_bytes {
        let window = &remaining[..max_bytes];
        // Prefer splitting at a double newline (paragraph break).
        let split_at = window
            .rfind("\n\n")
            .map(|i| i + 2)
            // Fall back to single newline.
            .or_else(|| window.rfind('\n').map(|i| i + 1))
            // Last resort: hard split at a char boundary.
            .unwrap_or_else(|| {
                let mut end = max_bytes;
                while end > 0 && !remaining.is_char_boundary(end) {
                    end -= 1;
                }
                end
            });
        parts.push(remaining[..split_at].to_string());
        remaining = &remaining[split_at..];
    }
    if !remaining.is_empty() {
        parts.push(remaining.to_string());
    }
    parts
}

/// Split a UTF-8 string into chunks that each fit within `max_bytes`.
fn split_utf8_chunks(text: &str, max_bytes: usize) -> Vec<&str> {
    if text.len() <= max_bytes {
        return vec![text];
    }
    let mut result = Vec::new();
    let mut offset = 0;
    while offset < text.len() {
        let mut end = (offset + max_bytes).min(text.len());
        // Avoid splitting a multi-byte UTF-8 sequence.
        while end > offset && !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == offset {
            // Should not happen for valid UTF-8, but fall back to exact byte.
            end = (offset + max_bytes).min(text.len());
        }
        result.push(&text[offset..end]);
        offset = end;
    }
    result
}

// ── InputSource + ImAdapter ──────────────────────────────────────────

#[async_trait]
impl InputSource for WechatChannel {
    fn id(&self) -> &str {
        "wechat"
    }

    fn kind(&self) -> InputSourceKind {
        InputSourceKind::Im
    }

    async fn start(
        &self,
    ) -> agentline_im_core::Result<
        tokio::sync::mpsc::Receiver<agentline_im_core::types::SourceMessage>,
    > {
        let ds = self
            .deferred_start
            .lock()
            .await
            .take()
            .ok_or_else(|| agentline_im_core::Error::other("wechat already started"))?;
        let cursor: CursorCell = Arc::new(RwLock::new(ds.initial_cursor));
        let (rx, _handle) = poll::spawn_poller(
            self.http.clone(),
            cursor,
            ds.persist,
            Arc::new(ds.allowed_users),
            self.registry.clone(),
            self.agent_done.clone(),
            32,
        );
        Ok(rx)
    }

    async fn send_update(&self, to: &PeerRef, event: &AgentEvent) -> agentline_im_core::Result<()> {
        let sid = event.session_id.to_string();
        let actions = {
            let mut states = self.render_states.lock().await;
            let st = states.entry(sid.clone()).or_default();
            synthesize(st, event.update.clone(), &event.tag)
        };
        let is_done = matches!(event.update, AgentUpdate::Done);
        for action in &actions {
            self.render_action(to, action).await?;
        }
        if is_done {
            self.render_states.lock().await.remove(&sid);
        }
        Ok(())
    }

    async fn shutdown(&self) -> agentline_im_core::Result<()> {
        self.shutdown_impl().await
    }
}

impl WechatChannel {
    async fn render_action(
        &self,
        to: &PeerRef,
        event: &OutboundEvent,
    ) -> agentline_im_core::Result<()> {
        use agentline_im_core::event::ToolEvent;
        use rust_i18n::t;

        let to = self.enrich_peer(to).await;
        let to = &to;
        let peer_id = &to.user_id;

        match event {
            OutboundEvent::PermissionRequest { .. }
            | OutboundEvent::ElicitInput { .. }
            | OutboundEvent::Done { .. } => {
                self.cancel_typing(to).await;
            }
            OutboundEvent::Thinking { .. }
            | OutboundEvent::StreamStart { .. }
            | OutboundEvent::StreamChunk { .. }
            | OutboundEvent::Tool(_) => {
                self.ensure_typing(to).await;
            }
            _ => {}
        }

        match event {
            OutboundEvent::Thinking { text, .. } => {
                let mut streams = self.active_streams.lock().await;
                let active = streams
                    .entry(peer_id.clone())
                    .or_insert_with(|| ActiveStream {
                        sender: None,
                        md_filter: markdown::StreamingMarkdownFilter::new(),
                        last_activity: std::time::Instant::now(),
                        signaled: false,
                        buffered_text: String::new(),
                        header_len: 0,
                        thinking_chars: 0,
                    });
                active.thinking_chars += text.chars().count();
                Ok(())
            }

            OutboundEvent::ThinkingEnd {
                tag, elapsed_secs, ..
            } => {
                let chars = {
                    let mut streams = self.active_streams.lock().await;
                    streams
                        .get_mut(peer_id)
                        .map(|a| std::mem::take(&mut a.thinking_chars))
                        .unwrap_or(0)
                };
                if chars > 0 {
                    let summary = t!(
                        "bridge.thinking_summary",
                        tag = tag,
                        secs = format!("{:.1}", elapsed_secs),
                        chars = chars
                    )
                    .to_string();
                    self.send_plain(to, &summary).await?;
                }
                Ok(())
            }

            OutboundEvent::StreamStart { tag } => {
                let header = format!("🤖 {tag} ");
                let mut streams = self.active_streams.lock().await;
                let active = streams
                    .entry(peer_id.clone())
                    .or_insert_with(|| ActiveStream {
                        sender: None,
                        md_filter: markdown::StreamingMarkdownFilter::new(),
                        last_activity: std::time::Instant::now(),
                        signaled: false,
                        buffered_text: String::new(),
                        header_len: 0,
                        thinking_chars: 0,
                    });
                if let Some(ref mut sender) = active.sender {
                    if !active.signaled
                        && let Some(ticket) = sender.ticket()
                    {
                        if let Err(e) = send::send_stream_signal(
                            &self.http,
                            to,
                            ticket,
                            sender.client_stream_id(),
                            "result",
                        )
                        .await
                        {
                            tracing::error!(error=%e, "send_stream_signal failed");
                        } else {
                            active.signaled = true;
                        }
                    }
                    let filtered = active.md_filter.feed(&header);
                    if !filtered.is_empty() {
                        for chunk in split_utf8_chunks(&filtered, MAX_PIECE_BYTES) {
                            let _ = sender
                                .send_piece(&stream::PiecePayload::Text {
                                    text: chunk.to_string(),
                                    stream_type: "result".to_string(),
                                })
                                .await;
                        }
                    }
                } else {
                    let filtered = active.md_filter.feed(&header);
                    active.buffered_text.push_str(&filtered);
                }
                Ok(())
            }

            OutboundEvent::StreamChunk { text } => {
                {
                    let batch = self.tool_batch.lock().await;
                    if !batch.lines.is_empty()
                        && batch.stream_buf.len() + text.len() <= BATCH_STREAM_ABSORB_MAX
                    {
                        drop(batch);
                        {
                            let mut streams = self.active_streams.lock().await;
                            if let Some(active) = streams.get_mut(peer_id)
                                && !active.buffered_text.is_empty()
                            {
                                let header = std::mem::take(&mut active.buffered_text);
                                let mut batch = self.tool_batch.lock().await;
                                batch.stream_buf.push_str(&header);
                            }
                        }
                        let batch_ref = self.tool_batch.clone();
                        let mut batch = self.tool_batch.lock().await;
                        batch.stream_buf.push_str(text);
                        Self::reset_batch_timer(
                            &mut batch,
                            batch_ref,
                            self.send_queue.clone(),
                            self.registry.clone(),
                        );
                        return Ok(());
                    }
                }
                self.flush_tool_batch().await;
                let mut streams = self.active_streams.lock().await;
                let active = match streams.get_mut(peer_id) {
                    Some(a) => {
                        a.last_activity = std::time::Instant::now();
                        a
                    }
                    None => {
                        let ts = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis())
                            .unwrap_or(0);
                        let client_stream_id = format!("{}:{}", self.device_id, ts);
                        let mut sender = stream::WeixinStreamSender::new(
                            self.http.clone(),
                            self.device_id.clone(),
                            client_stream_id,
                        );
                        let sender_opt = match sender.init().await {
                            Ok(()) => Some(sender),
                            Err(e) => {
                                tracing::warn!(error=%e, "native_init_stream failed; falling back to buffered text mode");
                                None
                            }
                        };
                        streams.insert(
                            peer_id.clone(),
                            ActiveStream {
                                sender: sender_opt,
                                md_filter: markdown::StreamingMarkdownFilter::new(),
                                last_activity: std::time::Instant::now(),
                                signaled: false,
                                buffered_text: String::new(),
                                header_len: 0,
                                thinking_chars: 0,
                            },
                        );
                        streams.get_mut(peer_id).unwrap()
                    }
                };

                if let Some(ref mut sender) = active.sender {
                    if !active.signaled
                        && let Some(ticket) = sender.ticket()
                    {
                        if let Err(e) = send::send_stream_signal(
                            &self.http,
                            to,
                            ticket,
                            sender.client_stream_id(),
                            "result",
                        )
                        .await
                        {
                            tracing::error!(error=%e, "send_stream_signal failed");
                        } else {
                            active.signaled = true;
                        }
                    }
                    let filtered = active.md_filter.feed(text);
                    if !filtered.is_empty() {
                        for chunk in split_utf8_chunks(&filtered, MAX_PIECE_BYTES) {
                            if let Err(e) = sender
                                .send_piece(&stream::PiecePayload::Text {
                                    text: chunk.to_string(),
                                    stream_type: "result".to_string(),
                                })
                                .await
                            {
                                tracing::error!(error=%e, "send_piece failed");
                            }
                        }
                    }
                    return Ok(());
                }

                let filtered = active.md_filter.feed(text);
                active.buffered_text.push_str(&filtered);
                Ok(())
            }

            OutboundEvent::StreamEnd => {
                self.flush_tool_batch().await;
                if let Some(text) = self.end_stream_for(peer_id).await {
                    self.send_plain(to, &text).await?;
                }
                Ok(())
            }

            OutboundEvent::Text { content, .. } => {
                {
                    let batch = self.tool_batch.lock().await;
                    if !batch.lines.is_empty() {
                        drop(batch);
                        self.push_tool_line(to, content.clone()).await;
                        return Ok(());
                    }
                }
                self.flush_tool_batch().await;
                if let Some(buffered) = self.end_stream_for(peer_id).await {
                    self.send_plain(to, &buffered).await?;
                }
                self.send_plain(to, content).await
            }

            OutboundEvent::Media(_) => Err(agentline_im_core::Error::NotSupported),

            OutboundEvent::Tool(ToolEvent::Start { .. }) => Ok(()),

            OutboundEvent::Tool(ToolEvent::End {
                id,
                ok,
                summary,
                label,
                ..
            }) => {
                if let Some(buffered) = self.end_stream_for(peer_id).await {
                    self.send_plain(to, &buffered).await?;
                }
                let icon = if *ok { "✅" } else { "❌" };
                let line = match summary {
                    Some(s) if !s.is_empty() => format!("{} {}", icon, s),
                    _ => {
                        let status = if *ok {
                            t!("im.tool_done")
                        } else {
                            t!("im.tool_failed")
                        };
                        if label.is_empty() {
                            format!("{icon} {status}")
                        } else {
                            format!("{icon} {label}: {status}")
                        }
                    }
                };
                self.tool_batch.lock().await.progress.remove(id);
                self.push_tool_line(to, line).await;
                Ok(())
            }

            OutboundEvent::Tool(ToolEvent::Progress { id, output }) => {
                let mut batch = self.tool_batch.lock().await;
                batch
                    .progress
                    .entry(id.clone())
                    .or_default()
                    .push_str(output);
                batch.peer = Some(to.clone());
                Ok(())
            }

            OutboundEvent::PermissionRequest {
                what,
                danger,
                tool_kind,
                ..
            } => {
                self.flush_tool_batch().await;
                if let Some(buffered) = self.end_stream_for(peer_id).await {
                    self.send_plain(to, &buffered).await?;
                }
                let icon = match danger {
                    PermissionDanger::Low => "🟢",
                    PermissionDanger::Medium => "🟡",
                    PermissionDanger::High => "🔴",
                };
                let risk = match danger {
                    PermissionDanger::Low => t!("im.risk_low"),
                    PermissionDanger::Medium => t!("im.risk_medium"),
                    PermissionDanger::High => t!("im.risk_high"),
                };
                let kind_name = tool_kind.name();
                let text = t!(
                    "im.perm_request_full",
                    kind = kind_name,
                    what = what,
                    icon = icon,
                    risk = risk
                )
                .to_string();
                self.send_plain(to, &text).await
            }

            OutboundEvent::ElicitInput { prompt, schema, .. } => {
                self.flush_tool_batch().await;
                if let Some(buffered) = self.end_stream_for(peer_id).await {
                    self.send_plain(to, &buffered).await?;
                }
                let mut s = format!("💬 {prompt}");
                if let Some(schema) = schema {
                    if let Some((_, prop)) = schema.properties.iter().next() {
                        match prop {
                            ElicitationPropertySchema::String(sp) => {
                                if let Some(options) = single_select_options(sp) {
                                    s.push('\n');
                                    for (i, (_, label)) in options.iter().enumerate() {
                                        s.push_str(&format!("\n{}. {}", i + 1, label));
                                    }
                                    s.push_str(&t!("im.elicit_select_hint"));
                                } else {
                                    s.push_str(&t!("im.elicit_free_hint"));
                                }
                            }
                            ElicitationPropertySchema::Array(ms) => {
                                let options = multi_select_options(&ms.items);
                                s.push('\n');
                                for (i, (_, label)) in options.iter().enumerate() {
                                    s.push_str(&format!("\n{}. {}", i + 1, label));
                                }
                                s.push_str(&t!("im.elicit_multi_hint"));
                            }
                            ElicitationPropertySchema::Boolean(_) => {
                                s.push_str(&t!("im.elicit_bool_hint"));
                            }
                            _ => {
                                s.push_str(&t!("im.elicit_free_hint"));
                            }
                        }
                    } else {
                        s.push_str(&t!("im.elicit_free_hint"));
                    }
                } else {
                    s.push_str(&t!("im.elicit_free_hint"));
                }
                self.send_plain(to, &s).await
            }

            OutboundEvent::Plan { steps } => {
                self.flush_tool_batch().await;
                if let Some(buffered) = self.end_stream_for(peer_id).await {
                    self.send_plain(to, &buffered).await?;
                }
                let title = t!("im.plan_title");
                let mut text = format!("{title}\n\n");
                for (i, step) in steps.iter().enumerate() {
                    text.push_str(&format!("**{}.** {}\n", i + 1, step));
                }
                self.send_plain(to, text.trim_end()).await
            }

            OutboundEvent::ModeChanged { .. } | OutboundEvent::SessionTitle { .. } => {
                agentline_im_core::render_outbound_event(self, to, event).await
            }

            OutboundEvent::Done { silent } => {
                self.flush_tool_batch().await;
                self.agent_done.lock().await.insert(peer_id.clone());
                if *silent {
                    self.send_plain(to, &t!("im.stream_done")).await?;
                }
                Ok(())
            }

            OutboundEvent::Error(msg) => {
                self.flush_tool_batch().await;
                if let Some(buffered) = self.end_stream_for(peer_id).await {
                    self.send_plain(to, &buffered).await?;
                }
                let text = t!("im.error_prefix", msg = msg).to_string();
                self.send_plain(to, &text).await
            }
        }
    }

    async fn shutdown_impl(&self) -> agentline_im_core::Result<()> {
        Ok(())
    }
}

#[async_trait]
impl ImAdapter for WechatChannel {
    async fn send_text(&self, to: &PeerRef, text: &str) -> agentline_im_core::Result<()> {
        self.send_plain(to, text).await
    }

    async fn typing(&self, to: &PeerRef) -> agentline_im_core::Result<()> {
        if self.registry.is_exhausted(&to.user_id) {
            return Ok(());
        }
        send::send_typing(&self.http, to, 1)
            .await
            .map_err(Into::into)
    }

    fn typing_interval(&self) -> Duration {
        Duration::from_secs(4)
    }

    fn capabilities(&self) -> ImCapabilities {
        ImCapabilities::default()
    }
}
