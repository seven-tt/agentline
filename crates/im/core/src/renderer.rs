//! Stateful synthesis of the bridge's raw agent-update stream into render
//! actions.
//!
//! The bridge forwards [`AgentUpdate`] operations wrapping ACP
//! [`SessionUpdate`]s. This is where presentation is reconstructed: stream
//! lifecycle (start/chunk/end), thinking elapsed-time summaries, tool-call
//! formatting, and the turn-end marker. IM adapters keep one [`RenderState`]
//! per session and feed each [`AgentUpdate`] through [`synthesize`], then
//! render the resulting [`OutboundEvent`]s with their platform-specific logic.

use crate::event::{OutboundEvent, ToolEvent};
use agent_client_protocol::{ContentBlock, SessionUpdate, TextContent, ToolCallStatus};
use agentline_bridge::acp_mapping;
use agentline_bridge::format::truncate;
use agentline_bridge::types::{AgentUpdate, ToolKind};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// Per-session render state. Create with `RenderState::default()` and keep it
/// alive across the updates of one turn. Reset (or dropped) after a `Done`.
#[derive(Debug, Default)]
pub struct RenderState {
    thinking_start: Option<Instant>,
    stream_started: bool,
    has_visible_output: bool,
    tool_meta: HashMap<String, (ToolKind, String)>,
    /// Last label a `Tool::Start` was sent with, per tool id — re-fires only
    /// when the label actually changes (e.g. placeholder → resolved arg),
    /// so repeated `ToolCallUpdate`s for the same call don't duplicate it.
    tool_started: HashMap<String, String>,
    tool_ended: HashSet<String>,
}

impl RenderState {
    fn reset(&mut self) {
        self.thinking_start = None;
        self.stream_started = false;
        self.has_visible_output = false;
        self.tool_meta.clear();
        self.tool_started.clear();
        self.tool_ended.clear();
    }
}

/// Convert one [`AgentUpdate`] into zero or more [`OutboundEvent`] render
/// actions, advancing `state`. `tag` is the session display tag (`[#1 claude]`).
pub fn synthesize(state: &mut RenderState, update: AgentUpdate, tag: &str) -> Vec<OutboundEvent> {
    let mut out = Vec::new();

    let is_thinking_or_tool = matches!(
        update,
        AgentUpdate::Session(
            SessionUpdate::AgentThoughtChunk(_)
                | SessionUpdate::ToolCall(_)
                | SessionUpdate::ToolCallUpdate(_)
        )
    );
    if !is_thinking_or_tool && state.thinking_start.is_some() {
        let elapsed = state
            .thinking_start
            .take()
            .map(|s| s.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        out.push(OutboundEvent::ThinkingEnd {
            tag: tag.to_string(),
            elapsed_secs: elapsed,
        });
    }

    // A tool call interrupts the current text stream. Once it's done, the
    // next `AgentMessageChunk` starts a visually new response and needs its
    // own `StreamStart` (so the agent tag header isn't lost on the resumed
    // answer).
    if matches!(
        update,
        AgentUpdate::Session(SessionUpdate::ToolCall(_) | SessionUpdate::ToolCallUpdate(_))
    ) {
        state.stream_started = false;
    }

    match update {
        AgentUpdate::Session(su) => match su {
            SessionUpdate::AgentThoughtChunk(chunk) => {
                if let ContentBlock::Text(TextContent { text, .. }) = chunk.content {
                    if state.thinking_start.is_none() {
                        state.thinking_start = Some(Instant::now());
                    }
                    out.push(OutboundEvent::Thinking {
                        tag: tag.to_string(),
                        text,
                    });
                }
            }

            SessionUpdate::AgentMessageChunk(chunk) => {
                if let ContentBlock::Text(TextContent { text, .. }) = chunk.content
                    && !text.is_empty()
                {
                    state.has_visible_output = true;
                    if !state.stream_started {
                        state.stream_started = true;
                        out.push(OutboundEvent::StreamStart {
                            tag: tag.to_string(),
                        });
                    }
                    out.push(OutboundEvent::StreamChunk { text });
                }
            }

            SessionUpdate::ToolCall(tc) => {
                let id = tc.tool_call_id.0.to_string();
                let kind = acp_mapping::map_tool_kind(tc.kind);
                let label =
                    acp_mapping::tool_arg(tc.raw_input.as_ref(), &tc.locations).unwrap_or(tc.title);
                state.tool_meta.insert(id.clone(), (kind, label.clone()));
                if state.tool_started.get(&id) != Some(&label) {
                    state.tool_started.insert(id.clone(), label.clone());
                    out.push(OutboundEvent::Tool(ToolEvent::Start {
                        id: id.clone(),
                        kind,
                        label,
                    }));
                }
                match tc.status {
                    ToolCallStatus::Completed | ToolCallStatus::Failed => {
                        let ok = matches!(tc.status, ToolCallStatus::Completed);
                        if state.tool_ended.insert(id.clone()) {
                            let (kind, label) = state
                                .tool_meta
                                .remove(&id)
                                .unwrap_or((ToolKind::Other, String::new()));
                            out.push(OutboundEvent::Tool(ToolEvent::End {
                                id,
                                ok,
                                summary: None,
                                kind,
                                label,
                            }));
                        }
                    }
                    _ => {}
                }
            }

            SessionUpdate::ToolCallUpdate(tcu) => {
                let id = tcu.tool_call_id.0.to_string();

                let locations = tcu.fields.locations.clone().unwrap_or_default();
                if let Some(label) =
                    acp_mapping::tool_arg(tcu.fields.raw_input.as_ref(), &locations)
                        .or_else(|| tcu.fields.title.clone())
                {
                    let kind = tcu
                        .fields
                        .kind
                        .map(acp_mapping::map_tool_kind)
                        .unwrap_or(ToolKind::Other);
                    state.tool_meta.insert(id.clone(), (kind, label.clone()));
                    if state.tool_started.get(&id) != Some(&label) {
                        state.tool_started.insert(id.clone(), label.clone());
                        out.push(OutboundEvent::Tool(ToolEvent::Start {
                            id: id.clone(),
                            kind,
                            label,
                        }));
                    }
                }

                if let Some(ref content) = tcu.fields.content {
                    let text: String = content
                        .iter()
                        .filter_map(|c| {
                            let json = serde_json::to_value(c).ok()?;
                            json.get("text")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .or_else(|| {
                                    json.get("content")
                                        .and_then(|cc| cc.get("text"))
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string())
                                })
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !text.is_empty() {
                        out.push(OutboundEvent::Tool(ToolEvent::Progress {
                            id: id.clone(),
                            output: text,
                        }));
                    }
                }

                if let Some((ok, summary)) = acp_mapping::tool_call_update_status(&tcu)
                    && state.tool_ended.insert(id.clone())
                {
                    let (kind, label) = state
                        .tool_meta
                        .remove(&id)
                        .unwrap_or((ToolKind::Other, String::new()));
                    let summary = summary.map(|s| {
                        if s.starts_with("```") {
                            let inner = s.trim_start_matches('`').trim_end_matches('`').trim();
                            format!("```\n{inner}\n```")
                        } else if !ok && !s.is_empty() {
                            truncate(&s, 800)
                        } else {
                            s
                        }
                    });
                    out.push(OutboundEvent::Tool(ToolEvent::End {
                        id,
                        ok,
                        summary,
                        kind,
                        label,
                    }));
                }
            }

            SessionUpdate::Plan(plan) => {
                let steps = acp_mapping::plan_to_steps(&plan);
                if !steps.is_empty() {
                    out.push(OutboundEvent::Plan { steps });
                }
            }

            SessionUpdate::CurrentModeUpdate(u) => {
                out.push(OutboundEvent::ModeChanged {
                    tag: tag.to_string(),
                    mode_id: u.current_mode_id.0.to_string(),
                });
            }

            SessionUpdate::SessionInfoUpdate(u) => {
                if let Some(title) = u.title.take()
                    && !title.is_empty()
                {
                    out.push(OutboundEvent::SessionTitle {
                        tag: tag.to_string(),
                        title,
                    });
                }
            }

            _ => {}
        },

        AgentUpdate::PermissionRequest {
            id,
            what,
            danger,
            tool_kind,
        } => {
            out.push(OutboundEvent::PermissionRequest {
                id,
                what,
                danger,
                tool_kind,
            });
        }

        AgentUpdate::ElicitInput { id, prompt, schema } => {
            out.push(OutboundEvent::ElicitInput { id, prompt, schema });
        }

        AgentUpdate::Done => {
            out.push(OutboundEvent::StreamEnd);
            out.push(OutboundEvent::Done {
                silent: !state.has_visible_output,
            });
            state.reset();
        }

        AgentUpdate::Error(msg) => {
            out.push(OutboundEvent::Error(msg));
        }
    }

    out
}
