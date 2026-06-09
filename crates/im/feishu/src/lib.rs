//! Feishu (Lark) IM adapter for agentline.
//!
//! Uses HTTP event subscription to receive messages and REST API to send.
//! Authentication: app_id + app_secret → tenant_access_token (2h TTL, auto-refresh).
//!
//! Outbound strategy:
//! - Streaming text → interactive card (create + PATCH update)
//! - Summary / plan / error → post (rich text)
//! - Tool status / permission → plain text

pub mod auth;
pub mod error;
pub mod send;
pub mod types;
pub mod webhook;

pub use error::{Error, Result};
pub use webhook::WebhookConfig;

use agentline_bridge::ImChannel;
use agentline_bridge::PermissionDanger;
use agentline_bridge::types::{ElicitFieldType, Media, MessageEvent, PeerRef};
use async_trait::async_trait;
use auth::TokenManager;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct FeishuConfig {
    pub app_id: String,
    pub app_secret: String,
    pub verification_token: String,
    pub encrypt_key: String,
    pub webhook_bind: String,
    pub allowed_users: Vec<String>,
}

struct ActiveCard {
    message_id: String,
    accumulated_text: String,
}

pub struct FeishuChannel {
    http: reqwest::Client,
    token_mgr: TokenManager,
    active_cards: Arc<Mutex<HashMap<String, ActiveCard>>>,
}

impl FeishuChannel {
    pub async fn start(
        cfg: FeishuConfig,
    ) -> Result<(
        Self,
        tokio::sync::mpsc::Receiver<agentline_bridge::types::InboundMessage>,
        tokio::task::JoinHandle<()>,
    )> {
        let token_mgr = TokenManager::new(cfg.app_id, cfg.app_secret).await?;
        let _refresh_handle = token_mgr.clone().spawn_refresh();

        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| Error::http(format!("build http: {e}")))?;

        let webhook_cfg = WebhookConfig {
            verification_token: cfg.verification_token,
            encrypt_key: cfg.encrypt_key,
            allowed_users: cfg.allowed_users,
        };

        let (rx, webhook_handle) = webhook::spawn_webhook(cfg.webhook_bind, webhook_cfg, 32);

        Ok((
            Self {
                http,
                token_mgr,
                active_cards: Arc::new(Mutex::new(HashMap::new())),
            },
            rx,
            webhook_handle,
        ))
    }

    async fn finalize_card(&self, peer_id: &str) {
        let mut cards = self.active_cards.lock().await;
        if let Some(active) = cards.remove(peer_id) {
            let card_json =
                types::build_streaming_card(&active.accumulated_text, "finished");
            if let Err(e) =
                send::update_card(&self.http, &self.token_mgr, &active.message_id, &card_json)
                    .await
            {
                tracing::warn!(error=%e, "failed to finalize feishu card");
            }
        }
    }

    async fn send_plain(&self, to: &PeerRef, text: &str) -> agentline_bridge::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        send::send_text(&self.http, &self.token_mgr, &to.user_id, text)
            .await
            .map(|_| ())
            .map_err(Into::into)
    }

    async fn send_rich(&self, to: &PeerRef, title: &str, text: &str) -> agentline_bridge::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        send::send_post(&self.http, &self.token_mgr, &to.user_id, title, text)
            .await
            .map(|_| ())
            .map_err(Into::into)
    }
}

#[async_trait]
impl ImChannel for FeishuChannel {
    async fn send_text(&self, to: &PeerRef, text: &str) -> agentline_bridge::Result<()> {
        self.send_plain(to, text).await
    }

    async fn send_media(&self, _to: &PeerRef, _media: Media) -> agentline_bridge::Result<()> {
        Err(agentline_bridge::Error::NotSupported)
    }

    async fn send_event(
        &self,
        to: &PeerRef,
        event: &MessageEvent,
    ) -> agentline_bridge::Result<()> {
        let peer_id = &to.user_id;

        match event {
            MessageEvent::StreamChunk { text } => {
                let mut cards = self.active_cards.lock().await;
                match cards.get_mut(peer_id) {
                    Some(active) => {
                        active.accumulated_text.push_str(text);
                        let card_json = types::build_streaming_card(
                            &active.accumulated_text,
                            "processing",
                        );
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
                    }
                    None => {
                        let card_json = types::build_streaming_card(text, "processing");
                        match send::send_card(
                            &self.http,
                            &self.token_mgr,
                            peer_id,
                            &card_json,
                        )
                        .await
                        {
                            Ok(message_id) => {
                                cards.insert(
                                    peer_id.clone(),
                                    ActiveCard {
                                        message_id,
                                        accumulated_text: text.clone(),
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

            MessageEvent::StreamEnd => {
                self.finalize_card(peer_id).await;
                Ok(())
            }

            MessageEvent::PlainText(text) => {
                self.finalize_card(peer_id).await;
                self.send_rich(to, "回复", text).await
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
                self.finalize_card(peer_id).await;
                let mut text = String::new();
                for (i, step) in steps.iter().enumerate() {
                    text.push_str(&format!("{}. {}\n", i + 1, step));
                }
                self.send_rich(to, "📋 执行计划", text.trim_end()).await
            }

            MessageEvent::PermissionRequest { what, danger, tag, .. } => {
                self.finalize_card(peer_id).await;
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
                self.send_rich(to, "❌ 错误", msg).await
            }

            MessageEvent::Done => {
                self.finalize_card(peer_id).await;
                Ok(())
            }
        }
    }
}
