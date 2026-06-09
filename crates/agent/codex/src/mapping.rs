//! Translate codex app-server `ThreadEvent` into the neutral `AgentUpdate` enum.
//!
//! Codex's streaming model is **item-based**: each piece of work (a tool
//! call, an assistant message, a plan step) is a `ThreadItem` with stable
//! ID, and the same item passes through `Started → Updated → Completed`
//! phases as its state changes. To avoid duplicating text on every
//! Updated frame, we map text-bearing items at the `Started` and
//! `Completed` boundaries only.

use agentline_bridge::{AgentUpdate, ToolKind};
use codex_app_server_sdk::api::{
    CommandExecutionStatus, PatchApplyStatus, ThreadEvent, ThreadItem,
};

pub fn map_thread_event(event: ThreadEvent) -> Vec<AgentUpdate> {
    match event {
        ThreadEvent::ItemStarted { item } => map_item(item, Phase::Started),
        ThreadEvent::ItemUpdated { item } => map_item(item, Phase::Updated),
        ThreadEvent::ItemCompleted { item } => map_item(item, Phase::Completed),
        ThreadEvent::TurnFailed { error } => vec![AgentUpdate::Error(error.message)],
        ThreadEvent::Error { message } => vec![AgentUpdate::Error(message)],
        // ThreadStarted / TurnStarted / TurnCompleted are bookkeeping;
        // the bridge handles "done" via the explicit Done update.
        _ => Vec::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Started,
    Updated,
    Completed,
}

fn map_item(item: ThreadItem, phase: Phase) -> Vec<AgentUpdate> {
    match item {
        ThreadItem::AgentMessage(msg) => {
            // Emit text at Completed only — earlier phases may carry
            // incremental snapshots that would duplicate output.
            if phase == Phase::Completed && !msg.text.is_empty() {
                let is_final = msg.is_final_answer();
                vec![AgentUpdate::AssistantText {
                    delta: msg.text,
                    is_final,
                }]
            } else {
                Vec::new()
            }
        }
        ThreadItem::Plan(p) => {
            if phase == Phase::Completed {
                vec![AgentUpdate::Plan {
                    steps: vec![p.text],
                }]
            } else {
                Vec::new()
            }
        }
        ThreadItem::Reasoning(_) => Vec::new(),
        ThreadItem::CommandExecution(cmd) => {
            let id = cmd.id.clone();
            match phase {
                Phase::Started => vec![AgentUpdate::ToolCallStart {
                    id,
                    kind: ToolKind::Shell,
                    label: cmd.command,
                }],
                Phase::Updated => Vec::new(),
                Phase::Completed => {
                    let mut out = Vec::new();
                    if !cmd.aggregated_output.is_empty() {
                        out.push(AgentUpdate::ToolCallProgress {
                            id: id.clone(),
                            output_chunk: cmd.aggregated_output,
                        });
                    }
                    let ok = matches!(cmd.status, CommandExecutionStatus::Completed);
                    let summary = cmd.exit_code.map(|c| format!("exit={c}"));
                    out.push(AgentUpdate::ToolCallEnd { id, ok, summary });
                    out
                }
            }
        }
        ThreadItem::FileChange(fc) => {
            let id = fc.id.clone();
            let label = fc
                .changes
                .iter()
                .map(|c| c.path.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            match phase {
                Phase::Started => vec![AgentUpdate::ToolCallStart {
                    id,
                    kind: ToolKind::FileEdit,
                    label,
                }],
                Phase::Updated => Vec::new(),
                Phase::Completed => {
                    let ok = matches!(fc.status, PatchApplyStatus::Completed);
                    vec![AgentUpdate::ToolCallEnd {
                        id,
                        ok,
                        summary: None,
                    }]
                }
            }
        }
        ThreadItem::McpToolCall(t) => {
            if phase == Phase::Started {
                vec![AgentUpdate::ToolCallStart {
                    id: t.id,
                    kind: ToolKind::Other,
                    label: format!("mcp:{}/{}", t.server, t.tool),
                }]
            } else if phase == Phase::Completed {
                vec![AgentUpdate::ToolCallEnd {
                    id: t.id,
                    ok: true,
                    summary: None,
                }]
            } else {
                Vec::new()
            }
        }
        ThreadItem::DynamicToolCall(t) => {
            if phase == Phase::Started {
                vec![AgentUpdate::ToolCallStart {
                    id: t.id,
                    kind: ToolKind::Other,
                    label: t.tool,
                }]
            } else if phase == Phase::Completed {
                vec![AgentUpdate::ToolCallEnd {
                    id: t.id,
                    ok: true,
                    summary: None,
                }]
            } else {
                Vec::new()
            }
        }
        ThreadItem::WebSearch(w) => {
            if phase == Phase::Started {
                vec![AgentUpdate::ToolCallStart {
                    id: w.id,
                    kind: ToolKind::Web,
                    label: w.query,
                }]
            } else if phase == Phase::Completed {
                vec![AgentUpdate::ToolCallEnd {
                    id: w.id,
                    ok: true,
                    summary: None,
                }]
            } else {
                Vec::new()
            }
        }
        ThreadItem::TodoList(todos) => {
            if phase == Phase::Completed && !todos.items.is_empty() {
                let steps: Vec<String> = todos
                    .items
                    .iter()
                    .map(|t| {
                        if t.completed {
                            format!("✓ {}", t.text)
                        } else {
                            t.text.clone()
                        }
                    })
                    .collect();
                vec![AgentUpdate::Plan { steps }]
            } else {
                Vec::new()
            }
        }
        ThreadItem::Error(e) => vec![AgentUpdate::Error(e.message)],
        // CollabToolCall, ImageView, EnteredReviewMode, ExitedReviewMode,
        // ContextCompaction, UserMessage, Unknown — silently dropped for v1.
        _ => Vec::new(),
    }
}
