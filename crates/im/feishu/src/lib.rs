//! Feishu (Lark) IM adapter for agentline.
//!
//! Uses WebSocket long-connection to receive messages and REST API to send.
//! Authentication: app_id + app_secret → tenant_access_token (2h TTL, auto-refresh).
//!
//! Outbound strategy:
//! - Streaming text → interactive card (create + PATCH update)
//! - Summary / plan / error → post (rich text)
//! - Tool status / permission → plain text

rust_i18n::i18n!("../core/locales", fallback = "zh-CN");

pub mod auth;
pub mod error;
pub mod media;
pub mod send;
pub mod types;
pub mod ws;

pub use error::{Error, Result};

use agentline_im_core::event::OutboundEvent;
use agentline_im_core::source::{ImAdapter, ImCapabilities, InputSource, InputSourceKind};
use agentline_im_core::types::{
    AgentUpdate, ElicitationPropertySchema, PeerRef, multi_select_options, single_select_options,
};
use agentline_im_core::{AgentEvent, PermissionDanger, RenderState, synthesize};
use async_trait::async_trait;
use auth::TokenManager;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct FeishuConfig {
    pub app_id: String,
    pub app_secret: String,
    pub allowed_users: Vec<String>,
}

/// Minimum interval between card PATCH updates. Feishu rate-limits PATCH
/// requests; sending one per chunk causes visible stuttering.
const CARD_UPDATE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(300);

struct ActiveCard {
    message_id: String,
    accumulated_text: String,
    thinking_chars: usize,
    last_update: std::time::Instant,
    dirty: bool,
}

pub struct FeishuChannel {
    http: reqwest::Client,
    token_mgr: TokenManager,
    active_cards: Arc<Mutex<HashMap<String, ActiveCard>>>,
    /// Per-session render synthesis state, keyed by session id.
    render_states: Arc<Mutex<HashMap<String, RenderState>>>,
    /// Permission cards awaiting a button click, keyed by the card's own
    /// message_id. Shared with the ws layer (`ws::WsConfig::perm_cards`),
    /// which consumes an entry on first click and ignores any repeat click
    /// on the same card.
    perm_cards: Arc<Mutex<HashMap<String, types::PermCardEntry>>>,
    cfg: Mutex<Option<FeishuConfig>>,
}

impl FeishuChannel {
    pub async fn start(
        cfg: FeishuConfig,
    ) -> Result<(
        Self,
        tokio::sync::mpsc::Receiver<agentline_im_core::types::SourceMessage>,
        tokio::task::JoinHandle<()>,
    )> {
        let token_mgr = TokenManager::new(cfg.app_id.clone(), cfg.app_secret.clone()).await?;
        let _refresh_handle = token_mgr.clone().spawn_refresh();

        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| Error::http(format!("build http: {e}")))?;

        let perm_cards = Arc::new(Mutex::new(HashMap::new()));
        let ws_cfg = ws::WsConfig {
            app_id: cfg.app_id,
            app_secret: cfg.app_secret,
            allowed_users: cfg.allowed_users,
            buffer: 32,
            token_mgr: token_mgr.clone(),
            perm_cards: perm_cards.clone(),
        };

        let (rx, ws_handle) = ws::spawn_ws_stream(ws_cfg);

        Ok((
            Self {
                http,
                token_mgr,
                active_cards: Arc::new(Mutex::new(HashMap::new())),
                render_states: Arc::new(Mutex::new(HashMap::new())),
                perm_cards,
                cfg: Mutex::new(None),
            },
            rx,
            ws_handle,
        ))
    }

    /// Create a FeishuChannel that can be started later via `InputSource::start()`.
    pub async fn new(cfg: FeishuConfig) -> Result<Self> {
        let token_mgr = TokenManager::new(cfg.app_id.clone(), cfg.app_secret.clone()).await?;
        let _refresh_handle = token_mgr.clone().spawn_refresh();
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| Error::http(format!("build http: {e}")))?;
        Ok(Self {
            http,
            token_mgr,
            active_cards: Arc::new(Mutex::new(HashMap::new())),
            render_states: Arc::new(Mutex::new(HashMap::new())),
            perm_cards: Arc::new(Mutex::new(HashMap::new())),
            cfg: Mutex::new(Some(cfg)),
        })
    }

    async fn finalize_card(&self, peer_id: &str) {
        let mut cards = self.active_cards.lock().await;
        if let Some(active) = cards.remove(peer_id) {
            let card_json = types::build_streaming_card(&active.accumulated_text, "finished");
            if let Err(e) =
                send::update_card(&self.http, &self.token_mgr, &active.message_id, &card_json).await
            {
                tracing::warn!(error=%e, "failed to finalize feishu card");
            }
        }
    }

    async fn send_plain(&self, to: &PeerRef, text: &str) -> agentline_im_core::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        send::send_text(&self.http, &self.token_mgr, &to.user_id, text)
            .await
            .map(|_| ())
            .map_err(Into::into)
    }

    async fn send_rich(
        &self,
        to: &PeerRef,
        title: &str,
        text: &str,
        template: &str,
    ) -> agentline_im_core::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let card_json = types::build_card(title, text, template);
        send::send_card(&self.http, &self.token_mgr, &to.user_id, &card_json)
            .await
            .map(|_| ())
            .map_err(Into::into)
    }
}

#[async_trait]
impl InputSource for FeishuChannel {
    fn id(&self) -> &str {
        "feishu"
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
            .ok_or_else(|| agentline_im_core::Error::other("feishu already started"))?;
        let ws_cfg = ws::WsConfig {
            app_id: cfg.app_id,
            app_secret: cfg.app_secret,
            allowed_users: cfg.allowed_users,
            buffer: 32,
            token_mgr: self.token_mgr.clone(),
            perm_cards: self.perm_cards.clone(),
        };
        let (rx, _handle) = ws::spawn_ws_stream(ws_cfg);
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

impl FeishuChannel {
    async fn render_action(
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
                            let card_json =
                                types::build_streaming_card(&active.accumulated_text, "thinking");
                            if let Err(e) = send::update_card(
                                &self.http,
                                &self.token_mgr,
                                &active.message_id,
                                &card_json,
                            )
                            .await
                            {
                                tracing::warn!(error=%e, "feishu thinking card update failed");
                            }
                            active.last_update = std::time::Instant::now();
                            active.dirty = false;
                        } else {
                            active.dirty = true;
                        }
                    }
                    None => {
                        let header = format!("💭 *{tag}*\n");
                        let display = format!("{header}{text}");
                        let card_json = types::build_streaming_card(&display, "thinking");
                        match send::send_card(&self.http, &self.token_mgr, peer_id, &card_json)
                            .await
                        {
                            Ok(message_id) => {
                                cards.insert(
                                    peer_id.clone(),
                                    ActiveCard {
                                        message_id,
                                        accumulated_text: display,
                                        thinking_chars: text.chars().count(),
                                        last_update: std::time::Instant::now(),
                                        dirty: false,
                                    },
                                );
                            }
                            Err(e) => {
                                tracing::error!(error=%e, "failed to create feishu thinking card");
                            }
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
                    let card_json =
                        types::build_streaming_card(&active.accumulated_text, "processing");
                    if let Err(e) = send::update_card(
                        &self.http,
                        &self.token_mgr,
                        &active.message_id,
                        &card_json,
                    )
                    .await
                    {
                        tracing::warn!(error=%e, "feishu thinking-end card update failed");
                    }
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
                    let card_json =
                        types::build_streaming_card(&active.accumulated_text, "processing");
                    if let Err(e) = send::update_card(
                        &self.http,
                        &self.token_mgr,
                        &active.message_id,
                        &card_json,
                    )
                    .await
                    {
                        tracing::warn!(error=%e, "feishu stream-start card update failed");
                    }
                } else {
                    let card_json = types::build_streaming_card(&header, "processing");
                    match send::send_card(&self.http, &self.token_mgr, peer_id, &card_json).await {
                        Ok(message_id) => {
                            cards.insert(
                                peer_id.clone(),
                                ActiveCard {
                                    message_id,
                                    accumulated_text: header,
                                    thinking_chars: 0,
                                    last_update: std::time::Instant::now(),
                                    dirty: false,
                                },
                            );
                        }
                        Err(e) => {
                            tracing::error!(error=%e, "failed to create feishu card for header");
                        }
                    }
                }
                Ok(())
            }

            OutboundEvent::StreamChunk { text } => {
                let mut cards = self.active_cards.lock().await;
                match cards.get_mut(peer_id) {
                    Some(active) => {
                        active.accumulated_text.push_str(text);
                        if active.last_update.elapsed() >= CARD_UPDATE_INTERVAL {
                            let card_json =
                                types::build_streaming_card(&active.accumulated_text, "processing");
                            if let Err(e) = send::update_card(
                                &self.http,
                                &self.token_mgr,
                                &active.message_id,
                                &card_json,
                            )
                            .await
                            {
                                tracing::warn!(error=%e, "feishu card update failed");
                            }
                            active.last_update = std::time::Instant::now();
                            active.dirty = false;
                        } else {
                            active.dirty = true;
                        }
                    }
                    None => {
                        let card_json = types::build_streaming_card(text, "processing");
                        match send::send_card(&self.http, &self.token_mgr, peer_id, &card_json)
                            .await
                        {
                            Ok(message_id) => {
                                cards.insert(
                                    peer_id.clone(),
                                    ActiveCard {
                                        message_id,
                                        accumulated_text: text.clone(),
                                        thinking_chars: 0,
                                        last_update: std::time::Instant::now(),
                                        dirty: false,
                                    },
                                );
                            }
                            Err(e) => {
                                tracing::error!(error=%e, "failed to create feishu card; falling back to text");
                                drop(cards);
                                return self.send_plain(to, text).await;
                            }
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
                        self.send_rich(to, "🤖 Agent", content, "blue").await
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
                self.finalize_card(peer_id).await;
                let icon = if *ok { "✅" } else { "❌" };
                let kind_label = if !label.is_empty() {
                    label.clone()
                } else {
                    kind.name().to_string()
                };
                let status = if *ok {
                    t!("im.tool_done")
                } else {
                    t!("im.tool_failed")
                };
                let title = format!("{icon} {status}");
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
                        format!("**{kind_label}**\n{cleaned}")
                    }
                    _ => {
                        format!("`{kind_label}`")
                    }
                };
                let template = if *ok { "green" } else { "red" };
                self.send_rich(to, &title, &body, template).await
            }

            OutboundEvent::PermissionRequest {
                what,
                danger,
                tool_kind,
                ..
            } => {
                self.finalize_card(peer_id).await;
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
                let card_json = types::build_permission_card(kind_name, what, icon, &risk);
                let message_id =
                    send::send_card(&self.http, &self.token_mgr, peer_id, &card_json).await?;
                if !message_id.is_empty() {
                    tracing::debug!(message_id=%message_id, "feishu: stored perm card for update");
                    self.perm_cards.lock().await.insert(
                        message_id,
                        types::PermCardEntry {
                            kind: kind_name.to_string(),
                            what: what.clone(),
                            risk_icon: icon,
                            risk: risk.to_string(),
                        },
                    );
                }
                Ok(())
            }

            OutboundEvent::ElicitInput { prompt, schema, .. } => {
                self.finalize_card(peer_id).await;
                let mut text = format!("💬 {prompt}");
                if let Some(schema) = schema {
                    if let Some((_, prop)) = schema.properties.iter().next() {
                        match prop {
                            ElicitationPropertySchema::String(sp) => {
                                if let Some(options) = single_select_options(sp) {
                                    text.push('\n');
                                    for (i, (_, label)) in options.iter().enumerate() {
                                        text.push_str(&format!("\n{}. {}", i + 1, label));
                                    }
                                    text.push_str(&t!("im.elicit_select_hint"));
                                } else {
                                    text.push_str(&t!("im.elicit_free_hint"));
                                }
                            }
                            ElicitationPropertySchema::Array(ms) => {
                                let options = multi_select_options(&ms.items);
                                text.push('\n');
                                for (i, (_, label)) in options.iter().enumerate() {
                                    text.push_str(&format!("\n{}. {}", i + 1, label));
                                }
                                text.push_str(&t!("im.elicit_multi_hint"));
                            }
                            ElicitationPropertySchema::Boolean(_) => {
                                text.push_str(&t!("im.elicit_bool_hint"));
                            }
                            _ => {
                                text.push_str(&t!("im.elicit_free_hint"));
                            }
                        }
                    } else {
                        text.push_str(&t!("im.elicit_free_hint"));
                    }
                } else {
                    text.push_str(&t!("im.elicit_free_hint"));
                }
                self.send_plain(to, &text).await
            }

            OutboundEvent::Plan { steps } => {
                self.finalize_card(peer_id).await;
                let mut text = String::new();
                for (i, step) in steps.iter().enumerate() {
                    text.push_str(&format!("{}. {}\n", i + 1, step));
                }
                let title = t!("im.plan_title").to_string();
                self.send_rich(to, &title, text.trim_end(), "purple").await
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
                    let card_json = types::build_streaming_card(msg, "failed");
                    let _ = send::update_card(
                        &self.http,
                        &self.token_mgr,
                        &active.message_id,
                        &card_json,
                    )
                    .await;
                    return Ok(());
                }
                drop(cards);
                let title = t!("im.stream_failed").to_string();
                self.send_rich(to, &title, msg, "red").await
            }
        }
    }
}

#[async_trait]
impl ImAdapter for FeishuChannel {
    async fn send_text(&self, to: &PeerRef, text: &str) -> agentline_im_core::Result<()> {
        self.send_plain(to, text).await
    }

    async fn send_markdown(&self, to: &PeerRef, text: &str) -> agentline_im_core::Result<()> {
        self.send_rich(to, "🤖 Agentline", text, "indigo").await
    }

    /// Native fields-grid card instead of a markdown-table conversion —
    /// Feishu cards support a proper 2-column key/value layout directly.
    async fn send_session_info(
        &self,
        to: &PeerRef,
        info: Option<&agentline_im_core::types::SessionInfo>,
        fallback_markdown: &str,
    ) -> agentline_im_core::Result<()> {
        use rust_i18n::t;
        if info.is_none() {
            return self.send_plain(to, &t!("bridge.session_list_empty")).await;
        };
        let title = "📋 会话信息";
        let card = types::build_raw_md_card(title, fallback_markdown, "indigo");
        send::send_card(&self.http, &self.token_mgr, &to.user_id, &card)
            .await
            .map(|_| ())
            .map_err(Into::into)
    }

    /// Native fields-grid card for `/agent` — one row per status group.
    async fn send_agent_list(
        &self,
        to: &PeerRef,
        current: &str,
        agents: &[agentline_im_core::types::AgentInfo],
        _fallback_markdown: &str,
    ) -> agentline_im_core::Result<()> {
        use agentline_im_core::types::AgentStatus;
        use rust_i18n::t;
        let group = |status: AgentStatus, label: std::borrow::Cow<'_, str>| {
            let names: Vec<&str> = agents
                .iter()
                .filter(|a| a.status == status)
                .map(|a| a.name.as_str())
                .collect();
            if names.is_empty() {
                None
            } else {
                Some((label.to_string(), names.join("\n")))
            }
        };
        let mut rows = vec![(
            t!("bridge.agent_list_current").to_string(),
            current.to_string(),
        )];
        rows.extend(group(AgentStatus::Ready, t!("bridge.agent_list_ready")));
        rows.extend(group(
            AgentStatus::Installed,
            t!("bridge.agent_list_installed"),
        ));
        rows.extend(group(
            AgentStatus::NotInstalled,
            t!("bridge.agent_list_not_installed"),
        ));
        let card = types::build_fields_card("🤖 Agents", "indigo", &rows);
        send::send_card(&self.http, &self.token_mgr, &to.user_id, &card)
            .await
            .map(|_| ())
            .map_err(Into::into)
    }

    fn typing_interval(&self) -> Duration {
        Duration::from_secs(60)
    }

    fn capabilities(&self) -> ImCapabilities {
        ImCapabilities {
            markdown: true,
            ..Default::default()
        }
    }
}
