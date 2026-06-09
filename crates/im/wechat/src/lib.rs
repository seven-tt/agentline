//! WeChat iLink bot adapter for agentline.
//!
//! Talks to https://ilinkai.weixin.qq.com — Tencent's officially-released
//! personal-WeChat Bot HTTP API (the iLink protocol).

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

use agentline_bridge::ImChannel;
use agentline_bridge::PermissionDanger;
use agentline_bridge::types::{ElicitFieldType, Media, MessageEvent, PeerRef};
use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
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
const CONTINUE_HINT: &str = "\n\n—— 本轮消息已达微信单次回复上限，回复「继续」以接收剩余内容 ——";

/// Queued, token-bucket-paced sender for discrete text messages.
///
/// Outbound messages are pushed onto an unbounded channel; a single background
/// worker drains them. Each send must claim a slot from the [`TokenRegistry`]:
/// every inbound user message grants a budget of [`token::MAX_MSGS_PER_CONTEXT`]
/// sends on its `context_token`, and iLink rejects anything beyond that with
/// `ret=-2`. The worker spends tokens oldest-first; when the budget runs out it
/// appends [`CONTINUE_HINT`] to the final message and **blocks** (messages stay
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
                        format!("{text}{CONTINUE_HINT}")
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

/// `ImChannel` impl for iLink. Construct via `WechatChannel::start` which also
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
        tokio::sync::mpsc::Receiver<agentline_bridge::types::InboundMessage>,
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
            },
            rx,
            handle,
            cursor,
        )
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
    async fn send_plain(&self, to: &PeerRef, text: &str) -> agentline_bridge::Result<()> {
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
            if !lines.is_empty() {
                let text = lines.join("\n\n");
                if let Err(e) = self.send_plain(&peer, &text).await {
                    tracing::error!(error=%e, "flush_tool_batch send failed");
                }
            }
            if !stream_buf.is_empty()
                && let Err(e) = self.send_plain(&peer, &stream_buf).await
            {
                tracing::error!(error=%e, "flush_tool_batch stream_buf send failed");
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
                if !lines.is_empty() {
                    let text = lines.join("\n\n");
                    for chunk in split_plain_message(&text, MAX_PLAIN_BYTES) {
                        send_queue.enqueue(enriched.clone(), chunk);
                    }
                }
                if !stream_buf.is_empty() {
                    for chunk in split_plain_message(&stream_buf, MAX_PLAIN_BYTES) {
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

#[async_trait]
impl ImChannel for WechatChannel {
    async fn send_text(&self, to: &PeerRef, text: &str) -> agentline_bridge::Result<()> {
        self.send_plain(to, text).await
    }

    async fn typing(&self, to: &PeerRef) -> agentline_bridge::Result<()> {
        send::send_typing(&self.http, to).await.map_err(Into::into)
    }

    async fn send_media(&self, _to: &PeerRef, _media: Media) -> agentline_bridge::Result<()> {
        Err(agentline_bridge::Error::NotSupported)
    }

    async fn send_event(&self, to: &PeerRef, event: &MessageEvent) -> agentline_bridge::Result<()> {
        let to = self.enrich_peer(to).await;
        let to = &to;
        let peer_id = &to.user_id;

        match event {
            MessageEvent::StreamChunk { text } => {
                // When the tool batch is active, absorb short inter-tool text
                // instead of opening a separate stream bubble.
                {
                    let batch = self.tool_batch.lock().await;
                    if !batch.lines.is_empty()
                        && batch.stream_buf.len() + text.len() <= BATCH_STREAM_ABSORB_MAX
                    {
                        drop(batch);
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
                            Ok(()) => {
                                tracing::debug!("native_init_stream succeeded");
                                Some(sender)
                            }
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
                            },
                        );
                        streams.get_mut(peer_id).unwrap()
                    }
                };

                // Streaming mode: send signal + piece.
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

                // Buffered mode: accumulate filtered text.
                let filtered = active.md_filter.feed(text);
                active.buffered_text.push_str(&filtered);
                Ok(())
            }

            MessageEvent::StreamEnd => {
                self.flush_tool_batch().await;
                if let Some(text) = self.end_stream_for(peer_id).await {
                    self.send_plain(to, &text).await?;
                }
                Ok(())
            }

            MessageEvent::PlainText(text) => {
                // If the tool batch is active, absorb short text (thinking
                // summaries, permission auto-approve) into the batch.
                {
                    let batch = self.tool_batch.lock().await;
                    if !batch.lines.is_empty() {
                        drop(batch);
                        self.push_tool_line(to, text.clone()).await;
                        return Ok(());
                    }
                }
                self.flush_tool_batch().await;
                if let Some(buffered) = self.end_stream_for(peer_id).await {
                    self.send_plain(to, &buffered).await?;
                }
                self.send_plain(to, text).await
            }

            MessageEvent::ToolStart { .. } => {
                // Swallowed: the bridge only sends ToolEnd with a combined label.
                Ok(())
            }

            MessageEvent::ToolEnd { id, ok, summary } => {
                if let Some(buffered) = self.end_stream_for(peer_id).await {
                    self.send_plain(to, &buffered).await?;
                }
                let icon = if *ok { "✅" } else { "❌" };
                let line = match summary {
                    Some(s) if !s.is_empty() => format!("{} {}", icon, s),
                    _ => format!("{} 工具执行{}", icon, if *ok { "完成" } else { "失败" }),
                };
                // Discard any stored ToolProgress output — too verbose for IM.
                self.tool_batch.lock().await.progress.remove(id);
                self.push_tool_line(to, line).await;
                Ok(())
            }

            MessageEvent::PermissionRequest {
                what, danger, tag, ..
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
                    PermissionDanger::Low => "低风险",
                    PermissionDanger::Medium => "中等风险",
                    PermissionDanger::High => "高风险",
                };
                // Compact two-line format for IM.
                let text = format!(
                    "⚠️ {tag} 需授权: {}\n{icon} {risk} · y 单次 / s session级 / n 拒绝",
                    what
                );
                self.send_plain(to, &text).await
            }

            MessageEvent::Plan { steps } => {
                self.flush_tool_batch().await;
                if let Some(buffered) = self.end_stream_for(peer_id).await {
                    self.send_plain(to, &buffered).await?;
                }
                let mut text = String::from("📋 **执行计划**\n\n");
                for (i, step) in steps.iter().enumerate() {
                    text.push_str(&format!("**{}.** {}\n", i + 1, step));
                }
                self.send_plain(to, text.trim_end()).await
            }

            MessageEvent::Error(msg) => {
                self.flush_tool_batch().await;
                if let Some(buffered) = self.end_stream_for(peer_id).await {
                    self.send_plain(to, &buffered).await?;
                }
                let text = format!(
                    "❌ **执行出错**\n\n\
                    {}\n\n\
                    如需重试，请重新发送请求。",
                    msg
                );
                self.send_plain(to, &text).await
            }

            MessageEvent::ToolProgress { id, output } => {
                let mut batch = self.tool_batch.lock().await;
                batch
                    .progress
                    .entry(id.clone())
                    .or_default()
                    .push_str(output);
                batch.peer = Some(to.clone());
                Ok(())
            }
            MessageEvent::ElicitInput { prompt, schema, .. } => {
                self.flush_tool_batch().await;
                if let Some(buffered) = self.end_stream_for(peer_id).await {
                    self.send_plain(to, &buffered).await?;
                }
                let mut s = format!("💬 {prompt}");
                if let Some(fields) = schema {
                    for field in fields {
                        match &field.field_type {
                            ElicitFieldType::SingleSelect { options }
                            | ElicitFieldType::MultiSelect { options } => {
                                s.push('\n');
                                for (i, opt) in options.iter().enumerate() {
                                    s.push_str(&format!("\n{}. {}", i + 1, opt.label));
                                    if let Some(desc) = &opt.description {
                                        s.push_str(&format!("  ({})", desc));
                                    }
                                }
                                let hint = match &field.field_type {
                                    ElicitFieldType::MultiSelect { .. } => {
                                        "\n\n回复编号选择，多选用逗号分隔"
                                    }
                                    _ => "\n\n回复编号选择，或直接输入",
                                };
                                s.push_str(hint);
                            }
                            ElicitFieldType::Boolean => {
                                s.push_str("\n\n回复 y/n");
                            }
                            _ => {
                                s.push_str("\n\n请直接回复");
                            }
                        }
                    }
                } else {
                    s.push_str("\n\n请直接回复");
                }
                self.send_plain(to, &s).await
            }

            MessageEvent::Done => {
                self.flush_tool_batch().await;
                self.agent_done.lock().await.insert(peer_id.clone());
                Ok(())
            }
        }
    }
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
