//! DingTalk Stream API adapter for agentline.
//!
//! Speaks the Dingtalk gateway protocol directly — no third-party SDK.
//! Reference: https://github.com/open-dingtalk/dingtalk-stream-sdk-go
//! Inbound: WebSocket stream, CALLBACK topic `/v1.0/im/bot/messages/get`.
//! Outbound: per-message `sessionWebhook` HTTP POST.

rust_i18n::i18n!("../core/locales", fallback = "zh-CN");

pub mod auth;
pub mod error;
pub mod send;
pub mod stream;
pub mod types;

pub use auth::OpenParams;
pub use error::{Error, Result};
pub use stream::{StreamConfig, WebhookCache, spawn_stream, spawn_stream_with};

use agentline_im_core::event::OutboundEvent;
use agentline_im_core::source::{ImAdapter, ImCapabilities, InputSource, InputSourceKind};
use agentline_im_core::types::PeerRef;
use async_trait::async_trait;
use std::time::Duration;
use tokio::sync::Mutex;

/// Outbound side of the DingTalk adapter. Construct via `DingtalkChannel::start`,
/// which returns the channel together with the inbound mpsc receiver.
pub struct DingtalkChannel {
    http: reqwest::Client,
    webhooks: WebhookCache,
    cfg: Mutex<Option<StreamConfig>>,
}

impl DingtalkChannel {
    /// Build the channel and spawn the stream loop. Returns
    /// `(channel, inbound_rx, join_handle)`.
    pub fn start(
        cfg: StreamConfig,
    ) -> Result<(
        Self,
        tokio::sync::mpsc::Receiver<agentline_im_core::types::InboundMessage>,
        tokio::task::JoinHandle<()>,
    )> {
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| Error::other(format!("build http: {e}")))?;
        let (rx, webhooks, handle) = spawn_stream(cfg);
        Ok((
            Self {
                http,
                webhooks,
                cfg: Mutex::new(None),
            },
            rx,
            handle,
        ))
    }

    /// Create a DingtalkChannel that can be started later via `InputSource::start()`.
    pub fn new(cfg: StreamConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| Error::other(format!("build http: {e}")))?;
        let webhooks: WebhookCache =
            std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        Ok(Self {
            http,
            webhooks,
            cfg: Mutex::new(Some(cfg)),
        })
    }

    async fn send_plain(&self, to: &PeerRef, text: &str) -> agentline_im_core::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        send::send_text(&self.http, &self.webhooks, to, text)
            .await
            .map_err(Into::into)
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
            .ok_or_else(|| agentline_im_core::Error::other("dingtalk already started"))?;
        let (rx, _handle) = spawn_stream_with(cfg, self.webhooks.clone());
        Ok(rx)
    }

    async fn send_event(
        &self,
        to: &PeerRef,
        event: &OutboundEvent,
    ) -> agentline_im_core::Result<()> {
        use agentline_im_core::event::ToolEvent;
        use rust_i18n::t;

        match event {
            OutboundEvent::Thinking { .. } => Ok(()),
            OutboundEvent::ThinkingEnd {
                tag, elapsed_secs, ..
            } => {
                let summary =
                    t!("im.thinking_short", secs = format!("{:.1}", elapsed_secs),).to_string();
                self.send_plain(to, &format!("💭 {tag} {summary}")).await
            }
            OutboundEvent::StreamStart { tag } => self.send_plain(to, &format!("🤖 {tag} ")).await,
            OutboundEvent::StreamChunk { text } => self.send_plain(to, text).await,
            OutboundEvent::StreamEnd => Ok(()),
            OutboundEvent::Text { content, .. } => self.send_plain(to, content).await,
            OutboundEvent::Media(_) => Err(agentline_im_core::Error::NotSupported),
            OutboundEvent::Tool(ToolEvent::Start { kind, label, .. }) => {
                let text = agentline_im_core::format::tool_label(*kind, label);
                self.send_plain(to, &text).await
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
                use agentline_im_core::types::ElicitFieldType;
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
                            }
                            ElicitFieldType::Boolean => {
                                s.push_str(&t!("im.elicit_bool_hint"));
                            }
                            _ => {
                                s.push_str(&t!("im.elicit_free_hint"));
                            }
                        }
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
                            "📋 #{id} · {agent}\n\
                             🆔 {sid}\n\
                             📁 {cwd}\n\
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
                if *silent {
                    self.send_plain(to, &t!("im.stream_done")).await?;
                }
                Ok(())
            }
            OutboundEvent::Error(msg) => {
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
impl ImAdapter for DingtalkChannel {
    async fn send_text(&self, to: &PeerRef, text: &str) -> agentline_im_core::Result<()> {
        self.send_plain(to, text).await
    }

    fn typing_interval(&self) -> Duration {
        Duration::from_secs(60)
    }

    fn capabilities(&self) -> ImCapabilities {
        ImCapabilities::default()
    }
}
