use crate::event::{OutboundEvent, TextFormat, ToolEvent};
use agent_client_protocol::ElicitationPropertySchema;
use agentline_bridge::Result;
use agentline_bridge::format::truncate;
use agentline_bridge::permission::PermissionDanger;
use agentline_bridge::source::ImAdapter;
use agentline_bridge::types::{PeerRef, multi_select_options, single_select_options};
use rust_i18n::t;

/// Default outbound renderer for IM adapters. Degrades every `OutboundEvent`
/// into text via `ImAdapter::send_text` / `send_markdown`. Complex IMs
/// override `InputSource::send_event` and call this as a fallback.
pub async fn render_outbound_event(
    im: &(dyn ImAdapter + Sync),
    to: &PeerRef,
    event: &OutboundEvent,
) -> Result<()> {
    match event {
        OutboundEvent::Thinking { .. } => Ok(()),
        OutboundEvent::ThinkingEnd {
            tag, elapsed_secs, ..
        } => {
            let summary =
                t!("im.thinking_short", secs = format!("{:.1}", elapsed_secs),).to_string();
            im.send_text(to, &format!("💭 {tag} {summary}")).await
        }
        OutboundEvent::StreamStart { tag } => im.send_text(to, &format!("🤖 {tag} ")).await,
        OutboundEvent::StreamChunk { text } => im.send_text(to, text).await,
        OutboundEvent::StreamEnd => Ok(()),
        OutboundEvent::Text { content, format } => match format {
            TextFormat::Markdown => im.send_markdown(to, content).await,
            TextFormat::Plain => im.send_text(to, content).await,
        },
        OutboundEvent::Media(_media) => Ok(()),
        OutboundEvent::Tool(ToolEvent::Start { .. }) => Ok(()),
        OutboundEvent::Tool(ToolEvent::Progress { .. }) => Ok(()),
        OutboundEvent::Tool(ToolEvent::End {
            ok, summary, label, ..
        }) => {
            let mark = if *ok { "✅" } else { "❌" };
            let body = match summary {
                Some(s) if !s.is_empty() => format!("{mark} {s}"),
                _ => {
                    let status = if *ok {
                        t!("im.tool_done")
                    } else {
                        t!("im.tool_failed")
                    };
                    if label.is_empty() {
                        format!("{mark} {status}")
                    } else {
                        format!("{mark} {label}: {status}")
                    }
                }
            };
            im.send_text(to, &body).await
        }
        OutboundEvent::ModeChanged { tag, mode_id } => {
            let text = t!("bridge.mode_changed", tag = tag, mode = mode_id).to_string();
            im.send_text(to, &text).await
        }
        OutboundEvent::SessionTitle { tag, title } => {
            im.send_text(to, &format!("📋 {tag} {title}")).await
        }
        OutboundEvent::Plan { steps } => {
            let mut s = format!("{}:\n", t!("im.plan_title"));
            for (i, step) in steps.iter().enumerate() {
                s.push_str(&format!("{}. {}\n", i + 1, step));
            }
            im.send_text(to, s.trim_end()).await
        }
        OutboundEvent::PermissionRequest {
            what,
            danger,
            tool_kind,
            ..
        } => {
            let icon = match danger {
                PermissionDanger::Low => "🟢",
                PermissionDanger::Medium => "🟡",
                PermissionDanger::High => "🔴",
            };
            let kind_name = tool_kind.name();
            let text = t!(
                "im.perm_request_v2",
                kind = kind_name,
                what = what,
                icon = icon
            )
            .to_string();
            im.send_text(to, &text).await
        }
        OutboundEvent::ElicitInput { prompt, schema, .. } => {
            let mut s = format!("💬 {prompt}");
            if let Some(schema) = schema {
                if let Some((_, prop)) = schema.properties.iter().next() {
                    render_elicit_property(&mut s, prop);
                } else {
                    s.push_str(&t!("im.elicit_free_hint"));
                }
            } else {
                s.push_str(&t!("im.elicit_free_hint"));
            }
            im.send_text(to, &s).await
        }
        OutboundEvent::Done { silent } => {
            if *silent {
                im.send_text(to, &t!("im.stream_done")).await?;
            }
            Ok(())
        }
        OutboundEvent::Error(msg) => {
            let text = t!("im.error_prefix", msg = truncate(msg, 1500)).to_string();
            im.send_text(to, &text).await
        }
    }
}

fn render_elicit_property(s: &mut String, prop: &ElicitationPropertySchema) {
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
}
