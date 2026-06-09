use crate::error::{Error, Result};
use crate::format::{tool_label, truncate};
use crate::permission::PermissionDanger;
use crate::types::{ElicitFieldType, Media, MessageEvent, PeerRef};
use async_trait::async_trait;
use rust_i18n::t;

/// Outbound side of an IM platform.
///
/// Inbound is delivered over a separate `tokio::sync::mpsc::Receiver<InboundMessage>`
/// produced by the adapter at construction time. This split keeps the trait
/// object-safe and avoids dance-around-stream-ownership during `select!`.
#[async_trait]
pub trait ImChannel: Send + Sync + 'static {
    /// Send plain text to a peer.
    async fn send_text(&self, to: &PeerRef, text: &str) -> Result<()>;

    /// Optional typing indicator. Default is no-op so platforms that don't
    /// support it stay quiet.
    async fn typing(&self, _to: &PeerRef) -> Result<()> {
        Ok(())
    }

    /// Optional media send. Default returns NotSupported.
    async fn send_media(&self, _to: &PeerRef, _media: Media) -> Result<()> {
        Err(Error::NotSupported)
    }

    /// Process a neutral message event from the bridge.
    ///
    /// The default implementation degrades every event into plain text and calls
    /// `send_text`, guaranteeing backward compatibility for adapters that do not
    /// override this method.
    ///
    /// Streaming-capable adapters (e.g. WeChat iLink) should override this to
    /// implement platform-specific protocols such as piece streaming, structured
    /// cards, markdown filtering, etc.
    async fn send_event(&self, to: &PeerRef, event: &MessageEvent) -> Result<()> {
        let text = match event {
            MessageEvent::StreamChunk { text } => text.clone(),
            MessageEvent::StreamEnd => return Ok(()),
            MessageEvent::PlainText(text) => text.clone(),
            MessageEvent::ToolStart { kind, label, .. } => tool_label(*kind, &truncate(label, 200)),
            MessageEvent::ToolProgress { .. } => return Ok(()),
            MessageEvent::ToolEnd { ok, summary, .. } => {
                let mark = if *ok { "✅" } else { "❌" };
                let default = if *ok {
                    t!("im.tool_done")
                } else {
                    t!("im.tool_failed")
                };
                let body = summary.as_deref().unwrap_or(&default);
                format!("{mark} {body}")
            }
            MessageEvent::Plan { steps } => {
                let mut s = format!("{}:\n", t!("im.plan_title"));
                for (i, step) in steps.iter().enumerate() {
                    s.push_str(&format!("{}. {}\n", i + 1, step));
                }
                s.trim_end().to_string()
            }
            MessageEvent::PermissionRequest {
                what, danger, tag, ..
            } => {
                let icon = match danger {
                    PermissionDanger::Low => "🟢",
                    PermissionDanger::Medium => "🟡",
                    PermissionDanger::High => "🔴",
                };
                t!("im.perm_request", tag = tag, what = what, icon = icon).to_string()
            }
            MessageEvent::ElicitInput { prompt, schema, .. } => {
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
                                        t!("im.elicit_multi_hint")
                                    }
                                    _ => t!("im.elicit_select_hint"),
                                };
                                s.push_str(&hint);
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
                s
            }
            MessageEvent::Done => return Ok(()),
            MessageEvent::Error(msg) => {
                t!("im.error_prefix", msg = truncate(msg, 1500)).to_string()
            }
        };
        self.send_text(to, &text).await
    }
}
