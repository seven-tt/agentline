//! Telegram Bot API adapter for agentline.
//!
//! Uses long-polling `getUpdates` to receive messages and REST API to send.
//! Authentication: single bot_token from BotFather (no refresh needed).
//!
//! Outbound strategy:
//! - Streaming text → sendMessage + editMessageText (edit-in-place)
//! - Summary / plan / error → sendMessage (plain text)
//! - Tool status / permission → sendMessage (short text)

pub mod error;
pub mod poll;
pub mod send;
pub mod types;

pub use error::{Error, Result};

use agentline_bridge::ImChannel;
use agentline_bridge::PermissionDanger;
use agentline_bridge::types::{ElicitFieldType, Media, MessageEvent, PeerRef};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
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
}

impl TelegramChannel {
    pub fn start(
        cfg: TelegramConfig,
    ) -> Result<(
        Self,
        tokio::sync::mpsc::Receiver<agentline_bridge::types::InboundMessage>,
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
            },
            rx,
            poll_handle,
        ))
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
            let text = format!("{}\n\n✅ 完成", active.accumulated_text);
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

    async fn send_plain(&self, to: &PeerRef, text: &str) -> agentline_bridge::Result<()> {
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
impl ImChannel for TelegramChannel {
    async fn send_text(&self, to: &PeerRef, text: &str) -> agentline_bridge::Result<()> {
        self.send_plain(to, text).await
    }

    async fn typing(&self, to: &PeerRef) -> agentline_bridge::Result<()> {
        let chat_id = Self::chat_id_from_peer(to);
        send::send_chat_action(&self.http, &self.api_base, &self.token, chat_id, "typing")
            .await
            .map_err(Into::into)
    }

    async fn send_media(&self, _to: &PeerRef, _media: Media) -> agentline_bridge::Result<()> {
        Err(agentline_bridge::Error::NotSupported)
    }

    async fn send_event(&self, to: &PeerRef, event: &MessageEvent) -> agentline_bridge::Result<()> {
        let peer_id = &to.user_id;
        let chat_id = Self::chat_id_from_peer(to);

        match event {
            MessageEvent::StreamChunk { text } => {
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

            MessageEvent::StreamEnd => {
                self.finalize_message(peer_id).await;
                Ok(())
            }

            MessageEvent::PlainText(text) => {
                self.finalize_message(peer_id).await;
                self.send_plain(to, text).await
            }

            MessageEvent::ToolStart { kind, label, .. } => {
                self.send_plain(to, &agentline_bridge::format::tool_label(*kind, label))
                    .await
            }

            MessageEvent::ToolEnd { ok, summary, .. } => {
                let icon = if *ok { "✅" } else { "❌" };
                let text = match summary {
                    Some(s) if !s.is_empty() => format!("{icon} {s}"),
                    _ => format!("{icon} 工具执行{}", if *ok { "完成" } else { "失败" }),
                };
                self.send_plain(to, &text).await
            }

            MessageEvent::ToolProgress { .. } => Ok(()),

            MessageEvent::Plan { steps } => {
                self.finalize_message(peer_id).await;
                let mut text = String::from("📋 执行计划:\n");
                for (i, step) in steps.iter().enumerate() {
                    text.push_str(&format!("{}. {}\n", i + 1, step));
                }
                self.send_plain(to, text.trim_end()).await
            }

            MessageEvent::PermissionRequest {
                what, danger, tag, ..
            } => {
                self.finalize_message(peer_id).await;
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
                let text = format!(
                    "⚠️ {tag} 需授权: {what}\n{icon} {risk} · y 单次 / s session级 / n 拒绝"
                );
                self.send_plain(to, &text).await
            }

            MessageEvent::ElicitInput { prompt, schema, .. } => {
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
                                        "\n\n（回复编号选择，多选用逗号分隔，如 1,3）"
                                    }
                                    _ => "\n\n（回复编号选择，或直接输入答案）",
                                };
                                text.push_str(hint);
                            }
                            ElicitFieldType::Boolean => {
                                text.push_str("\n\n（回复 y/n）");
                            }
                            _ => {
                                text.push_str("\n\n（请直接回复你的答案）");
                            }
                        }
                    }
                } else {
                    text.push_str("\n\n（请直接回复你的答案）");
                }
                self.send_plain(to, &text).await
            }

            MessageEvent::Error(msg) => {
                let mut messages = self.active_messages.lock().await;
                if let Some(active) = messages.remove(peer_id) {
                    let text = format!("❌ {msg}");
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
                self.send_plain(to, &format!("❌ {msg}")).await
            }

            MessageEvent::Done => {
                self.finalize_message(peer_id).await;
                Ok(())
            }
        }
    }
}
