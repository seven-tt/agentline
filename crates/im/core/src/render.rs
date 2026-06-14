use agentline_bridge::Result;
use agentline_bridge::event::{OutboundEvent, TextFormat, ToolEvent};
use agentline_bridge::format::{fmt_ago, fmt_local, truncate};
use agentline_bridge::permission::PermissionDanger;
use agentline_bridge::source::ImAdapter;
use agentline_bridge::types::{ElicitFieldType, PeerRef};
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
            im.send_text(to, &s).await
        }
        OutboundEvent::SessionList { info } => {
            let text = match info {
                None => t!("bridge.session_list_empty").to_string(),
                Some(s) => {
                    let perm = if s.is_yolo {
                        t!("bridge.yolo_label")
                    } else {
                        t!("bridge.safe_label")
                    };
                    format!(
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
                    )
                }
            };
            im.send_text(to, &text).await
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
