//! Telegram Bot API adapter for agentline.
//!
//! Uses long-polling `getUpdates` to receive messages and REST API to send.
//! Authentication: single bot_token from BotFather (no refresh needed).
//!
//! Outbound strategy (aligned with Feishu / DingTalk):
//! - Thinking → sendMessage + editMessageText (stream, expandable blockquote on finalize)
//! - StreamStart/Chunk/End → sendMessage + editMessageText (edit-in-place, 300ms throttle)
//! - Finalize → try Markdown parse_mode, fallback to plain text
//! - Tool / Permission / ElicitInput / Plan → HTML formatted messages
//! - Error → HTML formatted, or edit active message

rust_i18n::i18n!("../core/locales", fallback = "zh-CN");

pub mod error;
pub mod poll;
pub mod send;
pub mod types;

pub use error::{Error, Result};

use agentline_im_core::event::OutboundEvent;
use agentline_im_core::source::{ImAdapter, ImCapabilities, InputSource, InputSourceKind};
use agentline_im_core::types::{
    AgentUpdate, ElicitationPropertySchema, PeerRef, multi_select_options, single_select_options,
};
use agentline_im_core::{AgentEvent, PermissionDanger, RenderState, synthesize};
use async_trait::async_trait;
use send::escape_html;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

const DEFAULT_API_BASE: &str = "https://api.telegram.org";
const EDIT_INTERVAL: Duration = Duration::from_millis(300);

#[derive(Clone)]
pub struct TelegramConfig {
    pub bot_token: String,
    pub allowed_users: Vec<String>,
    pub api_base: String,
    pub proxy: String,
}

struct ActiveMessage {
    chat_id: i64,
    message_id: i64,
    accumulated_text: String,
    thinking_text: String,
    thinking_chars: usize,
    last_update: std::time::Instant,
    dirty: bool,
}

pub struct TelegramChannel {
    http: reqwest::Client,
    token: String,
    api_base: String,
    active_messages: Arc<Mutex<HashMap<String, ActiveMessage>>>,
    render_states: Arc<Mutex<HashMap<String, RenderState>>>,
    cfg: Mutex<Option<TelegramConfig>>,
}

impl TelegramChannel {
    pub fn start(
        cfg: TelegramConfig,
    ) -> Result<(
        Self,
        tokio::sync::mpsc::Receiver<agentline_im_core::types::SourceMessage>,
        tokio::task::JoinHandle<()>,
    )> {
        let http = build_http_client(&cfg.proxy)?;

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
                render_states: Arc::new(Mutex::new(HashMap::new())),
                cfg: Mutex::new(None),
            },
            rx,
            poll_handle,
        ))
    }

    pub fn new(cfg: TelegramConfig) -> Result<Self> {
        let http = build_http_client(&cfg.proxy)?;
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
            render_states: Arc::new(Mutex::new(HashMap::new())),
            cfg: Mutex::new(Some(cfg)),
        })
    }

    fn chat_id_from_peer(peer: &PeerRef) -> i64 {
        peer.opaque
            .get("chat_id")
            .and_then(|v| v.as_i64())
            .unwrap_or_else(|| peer.user_id.parse().unwrap_or(0))
    }

    // ── Send helpers ────────────────────────────────────────────────

    async fn edit_plain(&self, chat_id: i64, message_id: i64, text: &str) {
        if let Err(e) = send::edit_message_text(
            &self.http,
            &self.api_base,
            &self.token,
            chat_id,
            message_id,
            text,
            None,
        )
        .await
        {
            tracing::warn!(error=%e, "telegram edit failed");
        }
    }

    async fn edit_html(&self, chat_id: i64, message_id: i64, text: &str) {
        if let Err(e) = send::edit_message_text(
            &self.http,
            &self.api_base,
            &self.token,
            chat_id,
            message_id,
            text,
            Some("HTML"),
        )
        .await
        {
            tracing::debug!(error=%e, "HTML edit failed, retrying plain");
            self.edit_plain(chat_id, message_id, text).await;
        }
    }

    async fn finalize_message(&self, peer_id: &str) {
        let mut messages = self.active_messages.lock().await;
        let Some(active) = messages.remove(peer_id) else {
            return;
        };

        if active.dirty {
            self.edit_plain(active.chat_id, active.message_id, &active.accumulated_text)
                .await;
        }

        let thinking_block = if !active.thinking_text.is_empty() {
            // MarkdownV2 expandable blockquote: **> on each line, close with ||
            let escaped = telegramify_markdown::escape(&active.thinking_text);
            let lines: Vec<&str> = escaped.lines().collect();
            let mut bq = String::from("**>");
            for (i, line) in lines.iter().enumerate() {
                if i > 0 {
                    bq.push_str("\n>");
                }
                bq.push_str(line);
            }
            bq.push_str("||\n\n");
            bq
        } else {
            String::new()
        };

        let body = send::md_to_telegram_mdv2(&active.accumulated_text);
        let mdv2 = format!("{thinking_block}{body}");

        if send::edit_message_text(
            &self.http,
            &self.api_base,
            &self.token,
            active.chat_id,
            active.message_id,
            &mdv2,
            Some("MarkdownV2"),
        )
        .await
        .is_err()
        {
            let plain = if !active.thinking_text.is_empty() {
                format!("💭 {}\n\n{}", active.thinking_text, active.accumulated_text)
            } else {
                active.accumulated_text
            };
            self.edit_plain(active.chat_id, active.message_id, &plain)
                .await;
        }
    }

    async fn send_plain(&self, to: &PeerRef, text: &str) -> agentline_im_core::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let chat_id = Self::chat_id_from_peer(to);
        send::send_message(&self.http, &self.api_base, &self.token, chat_id, text, None)
            .await
            .map(|_| ())
            .map_err(Into::into)
    }

    async fn send_html(&self, to: &PeerRef, html: &str) -> agentline_im_core::Result<()> {
        self.send_html_with_markup(to, html, None).await
    }

    async fn send_html_with_markup(
        &self,
        to: &PeerRef,
        html: &str,
        markup: Option<types::InlineKeyboardMarkup>,
    ) -> agentline_im_core::Result<()> {
        if html.is_empty() {
            return Ok(());
        }
        let chat_id = Self::chat_id_from_peer(to);
        match send::send_message_with_markup(
            &self.http,
            &self.api_base,
            &self.token,
            chat_id,
            html,
            Some("HTML"),
            markup.clone(),
        )
        .await
        {
            Ok(_) => Ok(()),
            Err(_) => {
                tracing::debug!("HTML send failed, retrying plain");
                send::send_message_with_markup(
                    &self.http,
                    &self.api_base,
                    &self.token,
                    chat_id,
                    html,
                    None,
                    markup,
                )
                .await
                .map(|_| ())
                .map_err(Into::into)
            }
        }
    }

    async fn send_mdv2(&self, to: &PeerRef, mdv2: &str) -> agentline_im_core::Result<()> {
        if mdv2.is_empty() {
            return Ok(());
        }
        let chat_id = Self::chat_id_from_peer(to);
        match send::send_message(
            &self.http,
            &self.api_base,
            &self.token,
            chat_id,
            mdv2,
            Some("MarkdownV2"),
        )
        .await
        {
            Ok(_) => Ok(()),
            Err(_) => {
                tracing::debug!("MarkdownV2 send failed, retrying plain");
                send::send_message(&self.http, &self.api_base, &self.token, chat_id, mdv2, None)
                    .await
                    .map(|_| ())
                    .map_err(Into::into)
            }
        }
    }

    fn new_active(
        chat_id: i64,
        message_id: i64,
        text: String,
        thinking_chars: usize,
    ) -> ActiveMessage {
        ActiveMessage {
            chat_id,
            message_id,
            accumulated_text: text,
            thinking_text: String::new(),
            thinking_chars,
            last_update: std::time::Instant::now(),
            dirty: false,
        }
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

// ── Rendering ────────────────────────────────────────────────────────

impl TelegramChannel {
    async fn render_action(
        &self,
        to: &PeerRef,
        event: &OutboundEvent,
    ) -> agentline_im_core::Result<()> {
        use agentline_im_core::event::ToolEvent;
        use rust_i18n::t;

        let peer_id = &to.user_id;
        let chat_id = Self::chat_id_from_peer(to);

        match event {
            // ── Thinking ─────────────────────────────────────────
            OutboundEvent::Thinking { tag, text } => {
                let mut messages = self.active_messages.lock().await;
                match messages.get_mut(peer_id) {
                    Some(active) => {
                        active.thinking_chars += text.chars().count();
                        active.thinking_text.push_str(text);
                        active.accumulated_text.push_str(text);
                        if active.last_update.elapsed() >= EDIT_INTERVAL {
                            let display =
                                format!("💭 {} ...\n\n{}⏳", tag, active.accumulated_text);
                            self.edit_plain(active.chat_id, active.message_id, &display)
                                .await;
                            active.last_update = std::time::Instant::now();
                            active.dirty = false;
                        } else {
                            active.dirty = true;
                        }
                    }
                    None => {
                        let display = format!("💭 {tag} ...\n\n{text}⏳");
                        match send::send_message(
                            &self.http,
                            &self.api_base,
                            &self.token,
                            chat_id,
                            &display,
                            None,
                        )
                        .await
                        {
                            Ok(message_id) => {
                                let chars = text.chars().count();
                                let mut active =
                                    Self::new_active(chat_id, message_id, text.clone(), chars);
                                active.thinking_text = text.clone();
                                messages.insert(peer_id.clone(), active);
                            }
                            Err(e) => {
                                tracing::error!(error=%e, "telegram thinking send failed");
                            }
                        }
                    }
                }
                Ok(())
            }

            OutboundEvent::ThinkingEnd {
                tag, elapsed_secs, ..
            } => {
                let mut messages = self.active_messages.lock().await;
                if let Some(active) = messages.get_mut(peer_id) {
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
                            t!("im.thinking_short", secs = format!("{:.1}", elapsed_secs))
                        )
                    };
                    active.thinking_text = summary.clone();
                    active.accumulated_text = String::new();
                    active.thinking_chars = 0;
                    let display = format!("{summary}\n⏳");
                    self.edit_plain(active.chat_id, active.message_id, &display)
                        .await;
                    active.last_update = std::time::Instant::now();
                    active.dirty = false;
                    Ok(())
                } else {
                    drop(messages);
                    let summary =
                        t!("im.thinking_short", secs = format!("{:.1}", elapsed_secs)).to_string();
                    self.send_plain(to, &format!("💭 {tag} {summary}")).await
                }
            }

            // ── Streaming ────────────────────────────────────────
            OutboundEvent::StreamStart { tag } => {
                let header = format!("🤖 {tag} ");
                let mut messages = self.active_messages.lock().await;
                if let Some(active) = messages.get_mut(peer_id) {
                    active.accumulated_text = header.clone();
                    let display = if !active.thinking_text.is_empty() {
                        format!("{}\n\n{header}⏳", active.thinking_text)
                    } else {
                        format!("{header}⏳")
                    };
                    self.edit_plain(active.chat_id, active.message_id, &display)
                        .await;
                    active.last_update = std::time::Instant::now();
                    active.dirty = false;
                } else {
                    let display = format!("{header}⏳");
                    match send::send_message(
                        &self.http,
                        &self.api_base,
                        &self.token,
                        chat_id,
                        &display,
                        None,
                    )
                    .await
                    {
                        Ok(message_id) => {
                            messages.insert(
                                peer_id.clone(),
                                Self::new_active(chat_id, message_id, header, 0),
                            );
                        }
                        Err(e) => {
                            tracing::error!(error=%e, "telegram stream start failed");
                        }
                    }
                }
                Ok(())
            }

            OutboundEvent::StreamChunk { text } => {
                let mut messages = self.active_messages.lock().await;
                match messages.get_mut(peer_id) {
                    Some(active) => {
                        active.accumulated_text.push_str(text);
                        if active.last_update.elapsed() >= EDIT_INTERVAL {
                            let display = if !active.thinking_text.is_empty() {
                                format!("{}\n\n{}▌", active.thinking_text, active.accumulated_text)
                            } else {
                                format!("{}▌", active.accumulated_text)
                            };
                            self.edit_plain(active.chat_id, active.message_id, &display)
                                .await;
                            active.last_update = std::time::Instant::now();
                            active.dirty = false;
                        } else {
                            active.dirty = true;
                        }
                    }
                    None => {
                        let display = format!("{text}▌");
                        match send::send_message(
                            &self.http,
                            &self.api_base,
                            &self.token,
                            chat_id,
                            &display,
                            None,
                        )
                        .await
                        {
                            Ok(message_id) => {
                                messages.insert(
                                    peer_id.clone(),
                                    Self::new_active(chat_id, message_id, text.clone(), 0),
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

            // ── Text ─────────────────────────────────────────────
            OutboundEvent::Text { content, format } => {
                self.finalize_message(peer_id).await;
                match format {
                    // Telegram has no table/card support; a <pre> block at
                    // least renders pipe-tables and bold markers in a fixed
                    // font instead of as literal text noise.
                    agentline_im_core::event::TextFormat::Markdown => {
                        let mdv2 = send::md_to_telegram_mdv2(content);
                        self.send_mdv2(to, &mdv2).await
                    }
                    agentline_im_core::event::TextFormat::Plain => {
                        self.send_plain(to, content).await
                    }
                }
            }

            OutboundEvent::Media(_) => Err(agentline_im_core::Error::NotSupported),

            // ── Tool ─────────────────────────────────────────────
            OutboundEvent::Tool(ToolEvent::Start { .. }) => Ok(()),

            OutboundEvent::Tool(ToolEvent::Progress { .. }) => Ok(()),

            OutboundEvent::Tool(ToolEvent::End {
                ok,
                summary,
                label,
                kind,
                ..
            }) => {
                self.finalize_message(peer_id).await;
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
                let html = format!(
                    "{icon} <code>{}</code>: {}",
                    escape_html(&kind_label),
                    escape_html(&body)
                );
                self.send_html(to, &html).await
            }

            // ── Permission ───────────────────────────────────────
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
                let html = format!(
                    "{icon} <b>{}</b> <code>{}</code>\n<blockquote>{}</blockquote>",
                    escape_html(&risk),
                    escape_html(kind_name),
                    escape_html(what)
                );
                let markup = types::InlineKeyboardMarkup {
                    inline_keyboard: vec![vec![
                        types::InlineKeyboardButton {
                            text: "✅ 允许".into(),
                            callback_data: "perm:y".into(),
                        },
                        types::InlineKeyboardButton {
                            text: "✅ 本次会话".into(),
                            callback_data: "perm:s".into(),
                        },
                        types::InlineKeyboardButton {
                            text: "❌ 拒绝".into(),
                            callback_data: "perm:n".into(),
                        },
                    ]],
                };
                self.send_html_with_markup(to, &html, Some(markup)).await
            }

            // ── Elicit ───────────────────────────────────────────
            OutboundEvent::ElicitInput { prompt, schema, .. } => {
                self.finalize_message(peer_id).await;
                let mut html = format!("💬 <b>{}</b>", escape_html(prompt));
                if let Some(schema) = schema {
                    if let Some((_, prop)) = schema.properties.iter().next() {
                        match prop {
                            ElicitationPropertySchema::String(sp) => {
                                if let Some(options) = single_select_options(sp) {
                                    html.push('\n');
                                    for (i, (_, label)) in options.iter().enumerate() {
                                        html.push_str(&format!(
                                            "\n{}. {}",
                                            i + 1,
                                            escape_html(label)
                                        ));
                                    }
                                    html.push_str(&format!(
                                        "\n\n<i>{}</i>",
                                        escape_html(&t!("im.elicit_select_hint"))
                                    ));
                                } else {
                                    html.push_str(&format!(
                                        "\n\n<i>{}</i>",
                                        escape_html(&t!("im.elicit_free_hint"))
                                    ));
                                }
                            }
                            ElicitationPropertySchema::Array(ms) => {
                                let options = multi_select_options(&ms.items);
                                html.push('\n');
                                for (i, (_, label)) in options.iter().enumerate() {
                                    html.push_str(&format!("\n{}. {}", i + 1, escape_html(label)));
                                }
                                html.push_str(&format!(
                                    "\n\n<i>{}</i>",
                                    escape_html(&t!("im.elicit_multi_hint"))
                                ));
                            }
                            ElicitationPropertySchema::Boolean(_) => {
                                html.push_str(&format!(
                                    "\n\n<i>{}</i>",
                                    escape_html(&t!("im.elicit_bool_hint"))
                                ));
                            }
                            _ => {
                                html.push_str(&format!(
                                    "\n\n<i>{}</i>",
                                    escape_html(&t!("im.elicit_free_hint"))
                                ));
                            }
                        }
                    } else {
                        html.push_str(&format!(
                            "\n\n<i>{}</i>",
                            escape_html(&t!("im.elicit_free_hint"))
                        ));
                    }
                } else {
                    html.push_str(&format!(
                        "\n\n<i>{}</i>",
                        escape_html(&t!("im.elicit_free_hint"))
                    ));
                }
                self.send_html(to, &html).await
            }

            // ── Plan ─────────────────────────────────────────────
            OutboundEvent::Plan { steps } => {
                self.finalize_message(peer_id).await;
                let mut html = format!("📋 <b>{}</b>\n", escape_html(&t!("im.plan_title")));
                for (i, step) in steps.iter().enumerate() {
                    html.push_str(&format!("\n{}. {}", i + 1, escape_html(step)));
                }
                self.send_html(to, &html).await
            }

            OutboundEvent::ModeChanged { .. } | OutboundEvent::SessionTitle { .. } => {
                agentline_im_core::render_outbound_event(self, to, event).await
            }

            // ── Done ─────────────────────────────────────────────
            OutboundEvent::Done { .. } => {
                self.finalize_message(peer_id).await;
                Ok(())
            }

            // ── Error ────────────────────────────────────────────
            OutboundEvent::Error(msg) => {
                let mut messages = self.active_messages.lock().await;
                if let Some(active) = messages.remove(peer_id) {
                    let html = format!(
                        "❌ <b>Error</b>\n<blockquote>{}</blockquote>",
                        escape_html(msg)
                    );
                    self.edit_html(active.chat_id, active.message_id, &html)
                        .await;
                    return Ok(());
                }
                drop(messages);
                let html = format!(
                    "❌ <b>Error</b>\n<blockquote>{}</blockquote>",
                    escape_html(msg)
                );
                self.send_html(to, &html).await
            }
        }
    }
}

#[async_trait]
impl ImAdapter for TelegramChannel {
    async fn send_text(&self, to: &PeerRef, text: &str) -> agentline_im_core::Result<()> {
        self.send_plain(to, text).await
    }

    async fn send_markdown(&self, to: &PeerRef, text: &str) -> agentline_im_core::Result<()> {
        let mdv2 = send::md_to_telegram_mdv2(text);
        self.send_mdv2(to, &mdv2).await
    }

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
        let rows: Vec<(String, String)> = vec![
            (
                t!("bridge.session_id_label").to_string(),
                escape_html(&s.session_id),
            ),
            (
                t!("bridge.session_type_label").to_string(),
                escape_html(&s.agent_name),
            ),
            (
                t!("bridge.session_cwd_label").to_string(),
                escape_html(&s.cwd.display().to_string()),
            ),
            (
                t!("bridge.session_started_label").to_string(),
                escape_html(&fmt_local(s.created_at)),
            ),
            (
                t!("bridge.session_idle_label").to_string(),
                escape_html(&fmt_ago(s.idle_duration)),
            ),
            (
                t!("bridge.session_perm_label").to_string(),
                escape_html(&perm),
            ),
            (
                t!("bridge.session_grants_label").to_string(),
                escape_html(&s.grant_summary),
            ),
        ];
        let sep = "┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈";
        let mut body = String::new();
        for (i, (label, value)) in rows.iter().enumerate() {
            if i > 0 {
                body.push_str(sep);
                body.push('\n');
            }
            body.push_str(&format!("<b>{}</b>\n  {}\n", escape_html(label), value));
        }
        let html = format!(
            "📋 <b>#{id} · {agent}</b>\n\n{body}",
            id = s.short_id,
            agent = escape_html(&s.agent_name),
        );
        self.send_html(to, &html).await
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
            streaming: true,
            ..Default::default()
        }
    }
}

fn build_http_client(proxy: &str) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder();
    if !proxy.is_empty() {
        let p =
            reqwest::Proxy::all(proxy).map_err(|e| Error::Other(format!("invalid proxy: {e}")))?;
        builder = builder.proxy(p);
    }
    builder
        .build()
        .map_err(|e| Error::Other(format!("build http: {e}")))
}
