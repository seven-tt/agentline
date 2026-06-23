//! DingTalk Stream API adapter for agentline.
//!
//! Speaks the Dingtalk gateway protocol directly — no third-party SDK.
//! Reference: https://github.com/open-dingtalk/dingtalk-stream-sdk-go
//! Inbound: WebSocket stream, CALLBACK topic `/v1.0/im/bot/messages/get`.
//! Outbound: per-message `sessionWebhook` HTTP POST, or interactive card
//! streaming when `card_template_id` is configured.

rust_i18n::i18n!("../core/locales", fallback = "zh-CN");

pub mod auth;
pub mod error;
pub mod media;
pub mod send;
pub mod stream;
pub mod types;

pub use auth::{OpenParams, TokenManager};
pub use error::{Error, Result};
pub use stream::{StreamConfig, WebhookCache, spawn_stream, spawn_stream_with};

use agentline_im_core::event::OutboundEvent;
use agentline_im_core::source::{ImAdapter, ImCapabilities, InputSource, InputSourceKind};
use agentline_im_core::types::{AgentUpdate, PeerRef};
use agentline_im_core::{AgentEvent, RenderState, synthesize};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

const CARD_UPDATE_INTERVAL: Duration = Duration::from_millis(300);

struct ActiveCard {
    out_track_id: String,
    accumulated_text: String,
    thinking_chars: usize,
    last_update: std::time::Instant,
    dirty: bool,
}

/// Buffered plain-text stream (no-card fallback). `header` and `body` are
/// tracked separately so we can tell whether any real content ever arrived —
/// a tool call interrupting the stream before the first chunk shouldn't
/// flush a header-only message.
#[derive(Default)]
struct PlainStream {
    header: String,
    body: String,
}

/// Outbound side of the DingTalk adapter.
pub struct DingtalkChannel {
    http: reqwest::Client,
    webhooks: WebhookCache,
    /// Per-session render synthesis state, keyed by session id.
    render_states: Arc<Mutex<HashMap<String, RenderState>>>,
    /// Per-peer active streaming card, keyed by peer user_id.
    active_cards: Arc<Mutex<HashMap<String, ActiveCard>>>,
    /// Per-peer accumulated text for the plain-text (no-card) stream fallback,
    /// keyed by peer user_id. Webhooks can't edit a sent message, so unlike
    /// the card path we can't render incrementally — buffer the whole turn's
    /// text and send it as a single message on `StreamEnd`.
    plain_streams: Arc<Mutex<HashMap<String, PlainStream>>>,
    token_mgr: Option<TokenManager>,
    card_template_id: Option<String>,
    robot_code: String,
    cfg: Mutex<Option<StreamConfig>>,
}

impl DingtalkChannel {
    /// Build the channel and spawn the stream loop. Returns
    /// `(channel, inbound_rx, join_handle)`.
    pub async fn start(
        cfg: StreamConfig,
    ) -> Result<(
        Self,
        tokio::sync::mpsc::Receiver<agentline_im_core::types::SourceMessage>,
        tokio::task::JoinHandle<()>,
    )> {
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| Error::other(format!("build http: {e}")))?;

        let robot_code = cfg.open.client_id.clone();
        let (token_mgr, card_template_id) = init_card_support(
            &cfg.open.client_id,
            &cfg.open.client_secret,
            &cfg.card_template_id,
        )
        .await;

        let (rx, webhooks, handle) = spawn_stream(cfg);
        Ok((
            Self {
                http,
                webhooks,
                render_states: Arc::new(Mutex::new(HashMap::new())),
                active_cards: Arc::new(Mutex::new(HashMap::new())),
                plain_streams: Arc::new(Mutex::new(HashMap::new())),
                token_mgr,
                card_template_id,
                robot_code,
                cfg: Mutex::new(None),
            },
            rx,
            handle,
        ))
    }

    /// Create a DingtalkChannel that can be started later via `InputSource::start()`.
    pub async fn new(cfg: StreamConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| Error::other(format!("build http: {e}")))?;
        let webhooks: WebhookCache =
            std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));

        let robot_code = cfg.open.client_id.clone();
        let (token_mgr, card_template_id) = init_card_support(
            &cfg.open.client_id,
            &cfg.open.client_secret,
            &cfg.card_template_id,
        )
        .await;

        Ok(Self {
            http,
            webhooks,
            render_states: Arc::new(Mutex::new(HashMap::new())),
            active_cards: Arc::new(Mutex::new(HashMap::new())),
            plain_streams: Arc::new(Mutex::new(HashMap::new())),
            token_mgr,
            card_template_id,
            robot_code,
            cfg: Mutex::new(Some(cfg)),
        })
    }

    fn card_enabled(&self) -> bool {
        self.token_mgr.is_some() && self.card_template_id.is_some()
    }

    async fn send_plain(&self, to: &PeerRef, text: &str) -> agentline_im_core::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        send::send_text(&self.http, &self.webhooks, to, text)
            .await
            .map_err(Into::into)
    }

    /// `msgtype: "markdown"` webhook message — no card credentials needed.
    async fn send_markdown_text(
        &self,
        to: &PeerRef,
        title: &str,
        text: &str,
    ) -> agentline_im_core::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        send::send_markdown(&self.http, &self.webhooks, to, title, text)
            .await
            .map_err(Into::into)
    }

    // ── Card helpers ──────────────────────────────────────────────

    async fn create_card(
        &self,
        peer: &PeerRef,
        initial_content: &str,
        flow_status: &str,
    ) -> Option<String> {
        let token_mgr = self.token_mgr.as_ref()?;
        let tpl = self.card_template_id.as_deref()?;
        let out_track_id = send::gen_track_id();
        let params = send::CardDeliverParams {
            card_template_id: tpl,
            out_track_id: &out_track_id,
            robot_code: &self.robot_code,
            initial_content,
            flow_status,
        };
        match send::create_and_deliver_card(&self.http, token_mgr, peer, &params).await {
            Ok(()) => Some(out_track_id),
            Err(e) => {
                tracing::error!(error=%e, "failed to create dingtalk card");
                None
            }
        }
    }

    async fn stream_replace(&self, out_track_id: &str, content: &str) {
        let Some(tm) = &self.token_mgr else { return };
        let update = send::StreamUpdate {
            is_full: true,
            is_finalize: false,
            is_error: false,
        };
        if let Err(e) =
            send::streaming_update(&self.http, tm, out_track_id, "content", content, &update).await
        {
            tracing::warn!(error=%e, "dingtalk streaming replace failed");
        }
    }

    async fn stream_finalize(&self, out_track_id: &str) {
        let Some(tm) = &self.token_mgr else { return };
        let update = send::StreamUpdate {
            is_full: false,
            is_finalize: true,
            is_error: false,
        };
        if let Err(e) =
            send::streaming_update(&self.http, tm, out_track_id, "content", "", &update).await
        {
            tracing::warn!(error=%e, "dingtalk streaming finalize failed");
        }
    }

    async fn stream_error(&self, out_track_id: &str, msg: &str) {
        let Some(tm) = &self.token_mgr else { return };
        let update = send::StreamUpdate {
            is_full: true,
            is_finalize: true,
            is_error: true,
        };
        if let Err(e) =
            send::streaming_update(&self.http, tm, out_track_id, "content", msg, &update).await
        {
            tracing::warn!(error=%e, "dingtalk streaming error failed");
        }
    }

    async fn finalize_card(&self, peer_id: &str) {
        let mut cards = self.active_cards.lock().await;
        if let Some(active) = cards.remove(peer_id) {
            if active.dirty {
                self.stream_replace(&active.out_track_id, &active.accumulated_text)
                    .await;
            }
            self.stream_finalize(&active.out_track_id).await;
        }
    }
}

async fn init_card_support(
    client_id: &str,
    client_secret: &str,
    card_template_id: &str,
) -> (Option<TokenManager>, Option<String>) {
    if card_template_id.is_empty() {
        return (None, None);
    }
    match TokenManager::new(client_id.to_string(), client_secret.to_string()).await {
        Ok(mgr) => {
            let _refresh_handle = mgr.clone().spawn_refresh();
            tracing::info!(
                template_id = card_template_id,
                "dingtalk streaming card enabled"
            );
            (Some(mgr), Some(card_template_id.to_string()))
        }
        Err(e) => {
            tracing::error!(error=%e, "failed to init dingtalk TokenManager; cards disabled");
            (None, None)
        }
    }
}

#[async_trait]
impl InputSource for DingtalkChannel {
    fn id(&self) -> &str {
        "dingtalk"
    }

    fn kind(&self) -> InputSourceKind {
        InputSourceKind::Im
    }

    async fn start(
        &self,
    ) -> agentline_im_core::Result<
        tokio::sync::mpsc::Receiver<agentline_im_core::types::SourceMessage>,
    > {
        let cfg = self
            .cfg
            .lock()
            .await
            .take()
            .ok_or_else(|| agentline_im_core::Error::other("dingtalk already started"))?;
        let (rx, _handle) = spawn_stream_with(cfg, self.webhooks.clone());
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
        Ok(())
    }
}

// ── Rendering ─────────────────────────────────────────────────────

impl DingtalkChannel {
    async fn render_action(
        &self,
        to: &PeerRef,
        event: &OutboundEvent,
    ) -> agentline_im_core::Result<()> {
        if self.card_enabled() {
            self.render_card(to, event).await
        } else {
            self.render_text(to, event).await
        }
    }

    /// Card-based rendering — mirrors Feishu's streaming card logic.
    async fn render_card(
        &self,
        to: &PeerRef,
        event: &OutboundEvent,
    ) -> agentline_im_core::Result<()> {
        use agentline_im_core::event::ToolEvent;
        use rust_i18n::t;

        let peer_id = &to.user_id;

        match event {
            OutboundEvent::Thinking { tag, text } => {
                let mut cards = self.active_cards.lock().await;
                match cards.get_mut(peer_id) {
                    Some(active) => {
                        active.thinking_chars += text.chars().count();
                        active.accumulated_text.push_str(text);
                        if active.last_update.elapsed() >= CARD_UPDATE_INTERVAL {
                            self.stream_replace(&active.out_track_id, &active.accumulated_text)
                                .await;
                            active.last_update = std::time::Instant::now();
                            active.dirty = false;
                        } else {
                            active.dirty = true;
                        }
                    }
                    None => {
                        let header = format!("💭 *{tag}*\n");
                        let display = format!("{header}{text}");
                        if let Some(out_track_id) = self.create_card(to, &display, "1").await {
                            cards.insert(
                                peer_id.clone(),
                                ActiveCard {
                                    out_track_id,
                                    accumulated_text: display,
                                    thinking_chars: text.chars().count(),
                                    last_update: std::time::Instant::now(),
                                    dirty: false,
                                },
                            );
                        }
                    }
                }
                Ok(())
            }

            OutboundEvent::ThinkingEnd {
                tag, elapsed_secs, ..
            } => {
                let mut cards = self.active_cards.lock().await;
                if let Some(active) = cards.get_mut(peer_id) {
                    let chars = active.thinking_chars;
                    let summary = if chars > 0 {
                        t!(
                            "bridge.thinking_summary",
                            tag = tag,
                            secs = format!("{:.1}", elapsed_secs),
                            chars = chars
                        )
                        .to_string()
                    } else {
                        format!(
                            "💭 {tag} {}",
                            t!("im.thinking_short", secs = format!("{:.1}", elapsed_secs),)
                        )
                    };
                    active.accumulated_text = format!("{summary}\n---\n");
                    active.thinking_chars = 0;
                    self.stream_replace(&active.out_track_id, &active.accumulated_text)
                        .await;
                    active.last_update = std::time::Instant::now();
                    active.dirty = false;
                    Ok(())
                } else {
                    drop(cards);
                    let summary =
                        t!("im.thinking_short", secs = format!("{:.1}", elapsed_secs),).to_string();
                    self.send_plain(to, &format!("💭 {tag} {summary}")).await
                }
            }

            OutboundEvent::StreamStart { tag } => {
                let header = format!("🤖 {tag} ");
                let mut cards = self.active_cards.lock().await;
                if let Some(active) = cards.get_mut(peer_id) {
                    active.accumulated_text.push_str(&header);
                    self.stream_replace(&active.out_track_id, &active.accumulated_text)
                        .await;
                    active.last_update = std::time::Instant::now();
                    active.dirty = false;
                } else if let Some(out_track_id) = self.create_card(to, &header, "1").await {
                    cards.insert(
                        peer_id.clone(),
                        ActiveCard {
                            out_track_id,
                            accumulated_text: header,
                            thinking_chars: 0,
                            last_update: std::time::Instant::now(),
                            dirty: false,
                        },
                    );
                }
                Ok(())
            }

            OutboundEvent::StreamChunk { text } => {
                let mut cards = self.active_cards.lock().await;
                match cards.get_mut(peer_id) {
                    Some(active) => {
                        active.accumulated_text.push_str(text);
                        if active.last_update.elapsed() >= CARD_UPDATE_INTERVAL {
                            self.stream_replace(&active.out_track_id, &active.accumulated_text)
                                .await;
                            active.last_update = std::time::Instant::now();
                            active.dirty = false;
                        } else {
                            active.dirty = true;
                        }
                    }
                    None => {
                        if let Some(out_track_id) = self.create_card(to, text, "1").await {
                            cards.insert(
                                peer_id.clone(),
                                ActiveCard {
                                    out_track_id,
                                    accumulated_text: text.clone(),
                                    thinking_chars: 0,
                                    last_update: std::time::Instant::now(),
                                    dirty: false,
                                },
                            );
                        } else {
                            drop(cards);
                            return self.send_plain(to, text).await;
                        }
                    }
                }
                Ok(())
            }

            OutboundEvent::StreamEnd => {
                self.finalize_card(peer_id).await;
                Ok(())
            }

            OutboundEvent::Text { content, format } => {
                self.finalize_card(peer_id).await;
                match format {
                    agentline_im_core::event::TextFormat::Markdown => {
                        self.send_markdown_text(to, "🤖 Agentline", content).await
                    }
                    agentline_im_core::event::TextFormat::Plain => {
                        self.send_plain(to, content).await
                    }
                }
            }

            OutboundEvent::Media(_) => Err(agentline_im_core::Error::NotSupported),

            OutboundEvent::Tool(ToolEvent::Start { .. }) => Ok(()),

            OutboundEvent::Tool(ToolEvent::Progress { .. }) => Ok(()),

            OutboundEvent::Tool(ToolEvent::End {
                ok,
                summary,
                label,
                kind,
                ..
            }) => {
                let icon = if *ok { "✅" } else { "❌" };
                let kind_label = if !label.is_empty() {
                    label.clone()
                } else {
                    kind.name().to_string()
                };
                let body = match summary {
                    Some(s) if !s.is_empty() => {
                        let cleaned = s
                            .strip_prefix("```\n")
                            .or_else(|| s.strip_prefix("```"))
                            .and_then(|rest| {
                                rest.strip_suffix("\n```")
                                    .or_else(|| rest.strip_suffix("```"))
                            })
                            .unwrap_or(s);
                        cleaned.to_string()
                    }
                    _ => {
                        let status = if *ok {
                            t!("im.tool_done")
                        } else {
                            t!("im.tool_failed")
                        };
                        status.to_string()
                    }
                };
                let tool_text = format!("\n{icon} {kind_label}: {body}\n");
                let mut cards = self.active_cards.lock().await;
                if let Some(active) = cards.get_mut(peer_id) {
                    active.accumulated_text.push_str(&tool_text);
                    self.stream_replace(&active.out_track_id, &active.accumulated_text)
                        .await;
                    active.last_update = std::time::Instant::now();
                    active.dirty = false;
                    Ok(())
                } else {
                    drop(cards);
                    self.send_plain(to, &format!("{icon} {kind_label}: {body}"))
                        .await
                }
            }

            OutboundEvent::PermissionRequest {
                what,
                danger,
                tool_kind,
                ..
            } => {
                use agentline_im_core::PermissionDanger;
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
                use agentline_im_core::types::{
                    ElicitationPropertySchema, multi_select_options, single_select_options,
                };
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
                let mut text = format!("{}:\n", t!("im.plan_title"));
                for (i, step) in steps.iter().enumerate() {
                    text.push_str(&format!("{}. {}\n", i + 1, step));
                }
                self.send_plain(to, text.trim_end()).await
            }

            OutboundEvent::ModeChanged { .. } | OutboundEvent::SessionTitle { .. } => {
                agentline_im_core::render_outbound_event(self, to, event).await
            }

            OutboundEvent::Done { .. } => {
                self.finalize_card(peer_id).await;
                Ok(())
            }

            OutboundEvent::Error(msg) => {
                let mut cards = self.active_cards.lock().await;
                if let Some(active) = cards.remove(peer_id) {
                    self.stream_error(&active.out_track_id, msg).await;
                    return Ok(());
                }
                drop(cards);
                let text = t!("im.error_prefix", msg = msg).to_string();
                self.send_plain(to, &text).await
            }
        }
    }

    /// Plain text rendering — fallback when cards are not configured.
    async fn render_text(
        &self,
        to: &PeerRef,
        event: &OutboundEvent,
    ) -> agentline_im_core::Result<()> {
        use agentline_im_core::event::ToolEvent;
        use rust_i18n::t;

        let peer_id = &to.user_id;

        match event {
            OutboundEvent::Thinking { .. } => Ok(()),
            OutboundEvent::ThinkingEnd {
                tag, elapsed_secs, ..
            } => {
                let summary =
                    t!("im.thinking_short", secs = format!("{:.1}", elapsed_secs),).to_string();
                self.send_plain(to, &format!("💭 {tag} {summary}")).await
            }
            OutboundEvent::StreamStart { tag } => {
                self.plain_streams.lock().await.insert(
                    peer_id.clone(),
                    PlainStream {
                        header: format!("🤖 {tag} "),
                        body: String::new(),
                    },
                );
                Ok(())
            }
            OutboundEvent::StreamChunk { text } => {
                self.plain_streams
                    .lock()
                    .await
                    .entry(peer_id.clone())
                    .or_default()
                    .body
                    .push_str(text);
                Ok(())
            }
            OutboundEvent::StreamEnd => self.flush_plain_stream(to).await,
            OutboundEvent::Text { content, format } => {
                self.flush_plain_stream(to).await?;
                match format {
                    agentline_im_core::event::TextFormat::Markdown => {
                        self.send_markdown_text(to, "🤖 Agentline", content).await
                    }
                    agentline_im_core::event::TextFormat::Plain => {
                        self.send_plain(to, content).await
                    }
                }
            }
            OutboundEvent::Media(_) => Err(agentline_im_core::Error::NotSupported),
            OutboundEvent::Tool(ToolEvent::Start { .. }) => Ok(()),
            OutboundEvent::Tool(ToolEvent::Progress { .. }) => Ok(()),
            OutboundEvent::Tool(ToolEvent::End {
                ok, summary, label, ..
            }) => {
                self.flush_plain_stream(to).await?;
                let icon = if *ok { "✅" } else { "❌" };
                let text = match summary {
                    Some(s) if !s.is_empty() => format!("{icon} {s}"),
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
                self.send_markdown_text(to, &format!("{icon} 工具"), &text)
                    .await
            }
            OutboundEvent::PermissionRequest {
                what,
                danger,
                tool_kind,
                ..
            } => {
                use agentline_im_core::PermissionDanger;
                self.flush_plain_stream(to).await?;
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
                self.send_markdown_text(to, "⚠️ 需授权", &text).await
            }
            OutboundEvent::ElicitInput { prompt, schema, .. } => {
                use agentline_im_core::types::{
                    ElicitationPropertySchema, multi_select_options, single_select_options,
                };
                self.flush_plain_stream(to).await?;
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
                                }
                            }
                            ElicitationPropertySchema::Array(ms) => {
                                let options = multi_select_options(&ms.items);
                                s.push('\n');
                                for (i, (_, label)) in options.iter().enumerate() {
                                    s.push_str(&format!("\n{}. {}", i + 1, label));
                                }
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
                self.send_markdown_text(to, "💬 输入", &s).await
            }
            OutboundEvent::Plan { steps } => {
                self.flush_plain_stream(to).await?;
                let mut text = format!("{}:\n", t!("im.plan_title"));
                for (i, step) in steps.iter().enumerate() {
                    text.push_str(&format!("{}. {}\n", i + 1, step));
                }
                self.send_markdown_text(to, "📋 计划", text.trim_end())
                    .await
            }
            OutboundEvent::ModeChanged { .. } | OutboundEvent::SessionTitle { .. } => {
                agentline_im_core::render_outbound_event(self, to, event).await
            }
            OutboundEvent::Done { silent } => {
                self.flush_plain_stream(to).await?;
                if *silent {
                    self.send_plain(to, &t!("im.stream_done")).await?;
                }
                Ok(())
            }
            OutboundEvent::Error(msg) => {
                self.flush_plain_stream(to).await?;
                let text = t!("im.error_prefix", msg = msg).to_string();
                self.send_markdown_text(to, "❌ 错误", &text).await
            }
        }
    }

    /// Send and clear any buffered plain-text stream for `to`'s peer.
    /// No-op if no real content (beyond the header) was ever buffered — a
    /// tool call can interrupt a stream before its first chunk arrives, and
    /// a header-only message would just look empty.
    async fn flush_plain_stream(&self, to: &PeerRef) -> agentline_im_core::Result<()> {
        let buffered = self.plain_streams.lock().await.remove(&to.user_id);
        match buffered {
            Some(stream) if !stream.body.trim().is_empty() => {
                let title = stream.header.trim().to_string();
                let text = format!("{}{}", stream.header, stream.body);
                self.send_markdown_text(to, &title, &text).await
            }
            _ => Ok(()),
        }
    }
}

#[async_trait]
impl ImAdapter for DingtalkChannel {
    async fn send_text(&self, to: &PeerRef, text: &str) -> agentline_im_core::Result<()> {
        self.send_plain(to, text).await
    }

    /// `msgtype: "markdown"` doesn't need card credentials — available on
    /// every DingTalk webhook.
    async fn send_markdown(&self, to: &PeerRef, text: &str) -> agentline_im_core::Result<()> {
        self.send_markdown_text(to, "🤖 Agentline", text).await
    }

    /// DingTalk's markdown dialect has no table support, so the shared
    /// markdown-table fallback would show literal `|` characters — render
    /// bold-label / value lines instead.
    async fn send_session_info(
        &self,
        to: &PeerRef,
        info: Option<&agentline_im_core::types::SessionInfo>,
        _fallback_markdown: &str,
    ) -> agentline_im_core::Result<()> {
        use agentline_im_core::format::{fmt_ago, fmt_local};
        use rust_i18n::t;
        let Some(s) = info else {
            return self.send_plain(to, &t!("bridge.session_list_empty")).await;
        };
        let perm = if s.is_yolo {
            t!("bridge.yolo_label")
        } else {
            t!("bridge.safe_label")
        };
        let title = format!("📋 #{} · {}", s.short_id, s.agent_name);
        let text = format!(
            "### {title}\n\n\
             **{sid_l}**\n{sid}\n\n\
             **{type_l}**\n{agent}\n\n\
             **{cwd_l}**\n{cwd}\n\n\
             **{start_l}**\n{started}\n\n\
             **{idle_l}**\n{idle}\n\n\
             **{perm_l}**\n{perm}\n\n\
             **{grant_l}**\n{grants}",
            sid_l = t!("bridge.session_id_label"),
            sid = s.session_id,
            type_l = t!("bridge.session_type_label"),
            agent = s.agent_name,
            cwd_l = t!("bridge.session_cwd_label"),
            cwd = s.cwd.display(),
            start_l = t!("bridge.session_started_label"),
            started = fmt_local(s.created_at),
            idle_l = t!("bridge.session_idle_label"),
            idle = fmt_ago(s.idle_duration),
            perm_l = t!("bridge.session_perm_label"),
            perm = perm,
            grant_l = t!("bridge.session_grants_label"),
            grants = s.grant_summary,
        );
        self.send_markdown_text(to, &title, &text).await
    }

    fn typing_interval(&self) -> Duration {
        Duration::from_secs(60)
    }

    fn capabilities(&self) -> ImCapabilities {
        ImCapabilities {
            markdown: true,
            streaming: self.card_enabled(),
            cards: self.card_enabled(),
        }
    }
}
