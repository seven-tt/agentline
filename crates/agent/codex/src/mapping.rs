//! Translate codex app-server `ThreadEvent` into the neutral `AgentUpdate` enum.
//!
//! Codex's streaming model is **item-based**: each piece of work (a tool
//! call, an assistant message, a plan step) is a `ThreadItem` with stable
//! ID, and the same item passes through `Started → Updated → Completed`
//! phases as its state changes. To avoid duplicating text on every
//! Updated frame, we map text-bearing items at the `Started` and
//! `Completed` boundaries only.

use agentline_bridge::AgentUpdate;
use agentline_bridge::types::{
    AcpToolKind, ContentBlock, ContentChunk, Plan, PlanEntry, PlanEntryPriority, PlanEntryStatus,
    SessionUpdate, TextContent, ToolCall, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
};
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
        _ => Vec::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Started,
    Updated,
    Completed,
}

fn text_chunk(text: String) -> AgentUpdate {
    AgentUpdate::Session(SessionUpdate::AgentMessageChunk(ContentChunk::new(
        ContentBlock::Text(TextContent::new(text)),
    )))
}

fn tool_start(id: String, kind: AcpToolKind, label: String) -> AgentUpdate {
    AgentUpdate::Session(SessionUpdate::ToolCall(
        ToolCall::new(id, label)
            .kind(kind)
            .status(ToolCallStatus::InProgress),
    ))
}

fn tool_progress(id: String, output: String) -> AgentUpdate {
    let fields = ToolCallUpdateFields::new().raw_output(serde_json::Value::String(output));
    AgentUpdate::Session(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
        id, fields,
    )))
}

fn tool_end(id: String, ok: bool, summary: Option<String>) -> AgentUpdate {
    let status = if ok {
        ToolCallStatus::Completed
    } else {
        ToolCallStatus::Failed
    };
    let mut fields = ToolCallUpdateFields::new().status(status);
    if let Some(s) = summary {
        fields = fields.raw_output(serde_json::Value::String(s));
    }
    AgentUpdate::Session(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
        id, fields,
    )))
}

fn plan_update(steps: Vec<String>) -> AgentUpdate {
    let entries = steps
        .into_iter()
        .map(|s| PlanEntry::new(s, PlanEntryPriority::Medium, PlanEntryStatus::Pending))
        .collect();
    AgentUpdate::Session(SessionUpdate::Plan(Plan::new(entries)))
}

fn map_item(item: ThreadItem, phase: Phase) -> Vec<AgentUpdate> {
    match item {
        ThreadItem::AgentMessage(msg) => {
            if phase == Phase::Completed && !msg.text.is_empty() {
                vec![text_chunk(msg.text)]
            } else {
                Vec::new()
            }
        }
        ThreadItem::Plan(p) => {
            if phase == Phase::Completed {
                vec![plan_update(vec![p.text])]
            } else {
                Vec::new()
            }
        }
        ThreadItem::Reasoning(_) => Vec::new(),
        ThreadItem::CommandExecution(cmd) => {
            let id = cmd.id.clone();
            match phase {
                Phase::Started => vec![tool_start(id, AcpToolKind::Execute, cmd.command)],
                Phase::Updated => Vec::new(),
                Phase::Completed => {
                    let mut out = Vec::new();
                    if !cmd.aggregated_output.is_empty() {
                        out.push(tool_progress(id.clone(), cmd.aggregated_output));
                    }
                    let ok = matches!(cmd.status, CommandExecutionStatus::Completed);
                    let summary = cmd.exit_code.map(|c| format!("exit={c}"));
                    out.push(tool_end(id, ok, summary));
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
                Phase::Started => vec![tool_start(id, AcpToolKind::Edit, label)],
                Phase::Updated => Vec::new(),
                Phase::Completed => {
                    let ok = matches!(fc.status, PatchApplyStatus::Completed);
                    vec![tool_end(id, ok, None)]
                }
            }
        }
        ThreadItem::McpToolCall(t) => {
            if phase == Phase::Started {
                vec![tool_start(
                    t.id,
                    AcpToolKind::Other,
                    format!("mcp:{}/{}", t.server, t.tool),
                )]
            } else if phase == Phase::Completed {
                vec![tool_end(t.id, true, None)]
            } else {
                Vec::new()
            }
        }
        ThreadItem::DynamicToolCall(t) => {
            if phase == Phase::Started {
                vec![tool_start(t.id, AcpToolKind::Other, t.tool)]
            } else if phase == Phase::Completed {
                vec![tool_end(t.id, true, None)]
            } else {
                Vec::new()
            }
        }
        ThreadItem::WebSearch(w) => {
            if phase == Phase::Started {
                vec![tool_start(w.id, AcpToolKind::Fetch, w.query)]
            } else if phase == Phase::Completed {
                vec![tool_end(w.id, true, None)]
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
                vec![plan_update(steps)]
            } else {
                Vec::new()
            }
        }
        ThreadItem::Error(e) => vec![AgentUpdate::Error(e.message)],
        _ => Vec::new(),
    }
}
