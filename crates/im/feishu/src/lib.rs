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
pub mod send;
pub mod types;
pub mod ws;

pub use error::{Error, Result};

use agentline_im_core::PermissionDanger;
use agentline_im_core::event::OutboundEvent;
use agentline_im_core::source::{ImAdapter, ImCapabilities, InputSource, InputSourceKind};
use agentline_im_core::types::{ElicitFieldType, PeerRef};
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
    cfg: Mutex<Option<FeishuConfig>>,
}

impl FeishuChannel {
    pub async fn start(
        cfg: FeishuConfig,
    ) -> Result<(
        Self,
        tokio::sync::mpsc::Receiver<agentline_im_core::types::InboundMessage>,
        tokio::task::JoinHandle<()>,
    )> {
        let token_mgr = TokenManager::new(cfg.app_id.clone(), cfg.app_secret.clone()).await?;
        let _refresh_handle = token_mgr.clone().spawn_refresh();

        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| Error::http(format!("build http: {e}")))?;

        let ws_cfg = ws::WsConfig {
            app_id: cfg.app_id,
            app_secret: cfg.app_secret,
            allowed_users: cfg.allowed_users,
            buffer: 32,
        };

        let (rx, ws_handle) = ws::spawn_ws_stream(ws_cfg);

        Ok((
            Self {
                http,
                token_mgr,
                active_cards: Arc::new(Mutex::new(HashMap::new())),
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

    fn parse_message(
        &self,
        msg: &agentline_im_core::types::InboundMessage,
    ) -> agentline_im_core::types::InboundPayload {
        agentline_im_core::default_parse_message(msg)
    }

    async fn start(
        &self,
    ) -> agentline_im_core::Result<
        tokio::sync::mpsc::Receiver<agentline_im_core::types::InboundMessage>,
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
        };
        let (rx, _handle) = ws::spawn_ws_stream(ws_cfg);
        Ok(rx)
    }

    async fn send_event(
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
                let title = format!("{icon} {kind_label}");
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
                send::send_card(&self.http, &self.token_mgr, peer_id, &card_json)
                    .await
                    .map(|_| ())
                    .map_err(Into::into)
            }

            OutboundEvent::ElicitInput { prompt, schema, .. } => {
                self.finalize_card(peer_id).await;
                let mut text = format!("💬 {prompt}");
                if let Some(fields) = schema {
                    for field in fields {
                        match &field.field_type {
                            ElicitFieldType::SingleSelect { options }
                            | ElicitFieldType::MultiSelect { options } => {
                                text.push('\n');
                                for (i, opt) in options.iter().enumerate() {
                                    text.push_str(&format!("\n{}. {}", i + 1, opt.label));
                                    if let Some(desc) = &opt.description {
                                        text.push_str(&format!("  ({})", desc));
                                    }
                                }
                                let hint = match &field.field_type {
                                    ElicitFieldType::MultiSelect { .. } => {
                                        t!("im.elicit_multi_hint")
                                    }
                                    _ => t!("im.elicit_select_hint"),
                                };
                                text.push_str(&hint);
                            }
                            ElicitFieldType::Boolean => {
                                text.push_str(&t!("im.elicit_bool_hint"));
                            }
                            _ => {
                                text.push_str(&t!("im.elicit_free_hint"));
                            }
                        }
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

            OutboundEvent::SessionList { info } => {
                self.finalize_card(peer_id).await;
                match info {
                    None => self.send_plain(to, &t!("bridge.session_list_empty")).await,
                    Some(s) => {
                        let perm = if s.is_yolo {
                            t!("bridge.yolo_label")
                        } else {
                            t!("bridge.safe_label")
                        };
                        let content = format!(
                            "**🆔 Session ID**  `{sid}`\n\
                             **🤖 {type_l}**  {agent}\n\
                             **📁 {cwd_l}**  `{cwd}`\n\
                             **🕐 {start_l}**  {started}\n\
                             **⏱️ {idle_l}**  {idle}\n\
                             **🔐 {perm_l}**  {perm}\n\
                             **✅ {grant_l}**  {grants}",
                            sid = s.session_id,
                            type_l = t!("bridge.session_type_label"),
                            agent = s.agent_name,
                            cwd_l = t!("bridge.session_cwd_label"),
                            cwd = s.cwd.display(),
                            start_l = t!("bridge.session_started_label"),
                            started = agentline_im_core::format::fmt_local(s.created_at),
                            idle_l = t!("bridge.session_idle_label"),
                            idle = agentline_im_core::format::fmt_ago(s.idle_duration),
                            perm_l = t!("bridge.session_perm_label"),
                            perm = perm,
                            grant_l = t!("bridge.session_grants_label"),
                            grants = s.grant_summary,
                        );
                        let title = format!("📋 #{} · {}", s.short_id, s.agent_name);
                        self.send_rich(to, &title, &content, "indigo").await
                    }
                }
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

    async fn shutdown(&self) -> agentline_im_core::Result<()> {
        Ok(())
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
