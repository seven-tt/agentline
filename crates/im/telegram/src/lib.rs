//! Telegram Bot API adapter for agentline.
//!
//! Uses long-polling `getUpdates` to receive messages and REST API to send.
//! Authentication: single bot_token from BotFather (no refresh needed).
//!
//! Outbound strategy:
//! - Streaming text → sendMessage + editMessageText (edit-in-place)
//! - Summary / plan / error → sendMessage (plain text)
//! - Tool status / permission → sendMessage (short text)

rust_i18n::i18n!("../core/locales", fallback = "zh-CN");

pub mod error;
pub mod poll;
pub mod send;
pub mod types;

pub use error::{Error, Result};

use agentline_im_core::PermissionDanger;
use agentline_im_core::event::OutboundEvent;
use agentline_im_core::source::{ImAdapter, ImCapabilities, InputSource, InputSourceKind};
use agentline_im_core::types::{ElicitFieldType, PeerRef};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

const DEFAULT_API_BASE: &str = "https://api.telegram.org";

#[derive(Clone)]
pub struct TelegramConfig {
    pub bot_token: String,
    pub allowed_users: Vec<String>,
    pub api_base: String,
}

struct ActiveMessage {
    chat_id: i64,
    message_id: i64,
    accumulated_text: String,
}

pub struct TelegramChannel {
    http: reqwest::Client,
    token: String,
    api_base: String,
    active_messages: Arc<Mutex<HashMap<String, ActiveMessage>>>,
    cfg: Mutex<Option<TelegramConfig>>,
}

impl TelegramChannel {
    pub fn start(
        cfg: TelegramConfig,
    ) -> Result<(
        Self,
        tokio::sync::mpsc::Receiver<agentline_im_core::types::InboundMessage>,
        tokio::task::JoinHandle<()>,
    )> {
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| Error::Other(format!("build http: {e}")))?;

        let api_base = if cfg.api_base.is_empty() {
            DEFAULT_API_BASE.to_string()
        } else {
            cfg.api_base.trim_end_matches('/').to_string()
        };

        let (rx, poll_handle) = poll::spawn_poll(
            http.clone(),
            api_base.clone(),
            cfg.bot_token.clone(),
            cfg.allowed_users,
            32,
        );

        Ok((
            Self {
                http,
                token: cfg.bot_token,
                api_base,
                active_messages: Arc::new(Mutex::new(HashMap::new())),
                cfg: Mutex::new(None),
            },
            rx,
            poll_handle,
        ))
    }

    /// Create a TelegramChannel that can be started later via `InputSource::start()`.
    pub fn new(cfg: TelegramConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| Error::Other(format!("build http: {e}")))?;
        let api_base = if cfg.api_base.is_empty() {
            DEFAULT_API_BASE.to_string()
        } else {
            cfg.api_base.trim_end_matches('/').to_string()
        };
        Ok(Self {
            http,
            token: cfg.bot_token.clone(),
            api_base,
            active_messages: Arc::new(Mutex::new(HashMap::new())),
            cfg: Mutex::new(Some(cfg)),
        })
    }

    fn chat_id_from_peer(peer: &PeerRef) -> i64 {
        peer.opaque
            .get("chat_id")
            .and_then(|v| v.as_i64())
            .unwrap_or_else(|| peer.user_id.parse().unwrap_or(0))
    }

    async fn finalize_message(&self, peer_id: &str) {
        let mut messages = self.active_messages.lock().await;
        if let Some(active) = messages.remove(peer_id) {
            let done = rust_i18n::t!("im.stream_done");
            let text = format!("{}\n\n{done}", active.accumulated_text);
            if let Err(e) = send::edit_message_text(
                &self.http,
                &self.api_base,
                &self.token,
                active.chat_id,
                active.message_id,
                &text,
            )
            .await
            {
                tracing::warn!(error=%e, "failed to finalize telegram message");
            }
        }
    }

    async fn send_plain(&self, to: &PeerRef, text: &str) -> agentline_im_core::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let chat_id = Self::chat_id_from_peer(to);
        send::send_message(&self.http, &self.api_base, &self.token, chat_id, text)
            .await
            .map(|_| ())
            .map_err(Into::into)
    }
}

#[async_trait]
impl InputSource for TelegramChannel {
    fn id(&self) -> &str {
        "telegram"
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
            .ok_or_else(|| agentline_im_core::Error::other("telegram already started"))?;
        let (rx, _handle) = poll::spawn_poll(
            self.http.clone(),
            self.api_base.clone(),
            cfg.bot_token,
            cfg.allowed_users,
            32,
        );
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
        let chat_id = Self::chat_id_from_peer(to);

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
                let header = format!("🤖 {tag} ");
                let display = format!("{header}⏳");
                match send::send_message(&self.http, &self.api_base, &self.token, chat_id, &display)
                    .await
                {
                    Ok(message_id) => {
                        let mut messages = self.active_messages.lock().await;
                        messages.insert(
                            peer_id.clone(),
                            ActiveMessage {
                                chat_id,
                                message_id,
                                accumulated_text: header,
                            },
                        );
                    }
                    Err(e) => {
                        tracing::error!(error=%e, "telegram sendMessage header failed");
                    }
                }
                Ok(())
            }

            OutboundEvent::StreamChunk { text } => {
                let mut messages = self.active_messages.lock().await;
                match messages.get_mut(peer_id) {
                    Some(active) => {
                        active.accumulated_text.push_str(text);
                        let display = format!("{}⏳", active.accumulated_text);
                        if let Err(e) = send::edit_message_text(
                            &self.http,
                            &self.api_base,
                            &self.token,
                            active.chat_id,
                            active.message_id,
                            &display,
                        )
                        .await
                        {
                            tracing::warn!(error=%e, "telegram edit failed");
                        }
                    }
                    None => {
                        let display = format!("{text}⏳");
                        match send::send_message(
                            &self.http,
                            &self.api_base,
                            &self.token,
                            chat_id,
                            &display,
                        )
                        .await
                        {
                            Ok(message_id) => {
                                messages.insert(
                                    peer_id.clone(),
                                    ActiveMessage {
                                        chat_id,
                                        message_id,
                                        accumulated_text: text.clone(),
                                    },
                                );
                            }
                            Err(e) => {
                                tracing::error!(error=%e, "telegram sendMessage failed");
                                drop(messages);
                                return self.send_plain(to, text).await;
                            }
                        }
                    }
                }
                Ok(())
            }

            OutboundEvent::StreamEnd => {
                self.finalize_message(peer_id).await;
                Ok(())
            }

            OutboundEvent::Text { content, .. } => {
                self.finalize_message(peer_id).await;
                self.send_plain(to, content).await
            }

            OutboundEvent::Media(_) => Err(agentline_im_core::Error::NotSupported),

            OutboundEvent::Tool(ToolEvent::Start { kind, label, .. }) => {
                self.send_plain(to, &agentline_im_core::format::tool_label(*kind, label))
                    .await
            }

            OutboundEvent::Tool(ToolEvent::Progress { .. }) => Ok(()),

            OutboundEvent::Tool(ToolEvent::End {
                ok, summary, label, ..
            }) => {
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
                self.send_plain(to, &text).await
            }

            OutboundEvent::PermissionRequest {
                what,
                danger,
                tool_kind,
                ..
            } => {
                self.finalize_message(peer_id).await;
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
                self.finalize_message(peer_id).await;
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
                self.finalize_message(peer_id).await;
                let mut text = format!("{}:\n", t!("im.plan_title"));
                for (i, step) in steps.iter().enumerate() {
                    text.push_str(&format!("{}. {}\n", i + 1, step));
                }
                self.send_plain(to, text.trim_end()).await
            }

            OutboundEvent::SessionList { info } => {
                use agentline_im_core::format::{fmt_ago, fmt_local};
                match info {
                    None => self.send_plain(to, &t!("bridge.session_list_empty")).await,
                    Some(s) => {
                        let perm = if s.is_yolo {
                            t!("bridge.yolo_label")
                        } else {
                            t!("bridge.safe_label")
                        };
                        let text = format!(
                            "📋 *#{id}* · {agent}\n\
                             🆔 `{sid}`\n\
                             📁 `{cwd}`\n\
                             🕐 {started}\n\
                             ⏱️ {idle}\n\
                             🔐 {perm}\n\
                             ✅ {grants}",
                            id = s.short_id,
                            agent = s.agent_name,
                            sid = s.session_id,
                            cwd = s.cwd.display(),
                            started = fmt_local(s.created_at),
                            idle = fmt_ago(s.idle_duration),
                            perm = perm,
                            grants = s.grant_summary,
                        );
                        self.send_plain(to, &text).await
                    }
                }
            }
            OutboundEvent::ModeChanged { .. } | OutboundEvent::SessionTitle { .. } => {
                agentline_im_core::render_outbound_event(self, to, event).await
            }

            OutboundEvent::Done { silent } => {
                self.finalize_message(peer_id).await;
                if *silent {
                    self.send_text(to, &t!("im.stream_done")).await?;
                }
                Ok(())
            }

            OutboundEvent::Error(msg) => {
                let mut messages = self.active_messages.lock().await;
                if let Some(active) = messages.remove(peer_id) {
                    let text = t!("im.error_prefix", msg = msg).to_string();
                    let _ = send::edit_message_text(
                        &self.http,
                        &self.api_base,
                        &self.token,
                        active.chat_id,
                        active.message_id,
                        &text,
                    )
                    .await;
                    return Ok(());
                }
                drop(messages);
                let text = t!("im.error_prefix", msg = msg).to_string();
                self.send_plain(to, &text).await
            }
        }
    }

    async fn shutdown(&self) -> agentline_im_core::Result<()> {
        Ok(())
    }
}

#[async_trait]
impl ImAdapter for TelegramChannel {
    async fn send_text(&self, to: &PeerRef, text: &str) -> agentline_im_core::Result<()> {
        self.send_plain(to, text).await
    }

    async fn typing(&self, to: &PeerRef) -> agentline_im_core::Result<()> {
        let chat_id = Self::chat_id_from_peer(to);
        send::send_chat_action(&self.http, &self.api_base, &self.token, chat_id, "typing")
            .await
            .map_err(Into::into)
    }

    fn typing_interval(&self) -> Duration {
        Duration::from_secs(5)
    }

    fn capabilities(&self) -> ImCapabilities {
        ImCapabilities {
            markdown: true,
            ..Default::default()
        }
    }
}
