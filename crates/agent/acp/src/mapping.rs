//! Translate ACP protocol updates into the neutral `AgentUpdate` enum.

use agent_client_protocol::{
    ContentBlock, ContentChunk, CurrentModeUpdate, Plan, SessionInfoUpdate, SessionUpdate,
    TextContent, ToolCall as AcpToolCall, ToolCallContent, ToolCallLocation, ToolCallStatus,
    ToolCallUpdate as AcpToolCallUpdate, ToolKind as AcpToolKind,
};
use agentline_bridge::{AgentUpdate, ToolKind};

pub fn map_session_update(u: SessionUpdate) -> Vec<AgentUpdate> {
    match u {
        SessionUpdate::AgentMessageChunk(chunk) => chunk_to_text(chunk, false),
        SessionUpdate::AgentThoughtChunk(chunk) => {
            if let ContentBlock::Text(TextContent { text, .. }) = chunk.content {
                vec![AgentUpdate::Thinking { text }]
            } else {
                Vec::new()
            }
        }
        SessionUpdate::UserMessageChunk(_) => Vec::new(),
        SessionUpdate::ToolCall(tc) => tool_call_to_updates(tc),
        SessionUpdate::ToolCallUpdate(tcu) => tool_call_update_to_updates(tcu),
        SessionUpdate::Plan(plan) => plan_to_update(plan).map(|u| vec![u]).unwrap_or_default(),
        SessionUpdate::CurrentModeUpdate(u) => vec![mode_changed(u)],
        SessionUpdate::SessionInfoUpdate(u) => session_info_to_update(u)
            .map(|u| vec![u])
            .unwrap_or_default(),
        SessionUpdate::AvailableCommandsUpdate(_) => Vec::new(), // internal to agent, not shown to users
        _ => Vec::new(),
    }
}

fn chunk_to_text(chunk: ContentChunk, is_final: bool) -> Vec<AgentUpdate> {
    if let ContentBlock::Text(TextContent { text, .. }) = chunk.content {
        vec![AgentUpdate::AssistantText {
            delta: text,
            is_final,
        }]
    } else {
        Vec::new()
    }
}

/// Pull the most useful argument out of a tool call: the shell command, the
/// file path (read / edit / write / delete / move), the fetched url, or the
/// search query. Falls back to the first affected file location.
fn tool_arg(
    raw_input: Option<&serde_json::Value>,
    locations: &[ToolCallLocation],
) -> Option<String> {
    if let Some(raw) = raw_input {
        if let Some(cmd) = raw.get("command").and_then(|v| v.as_str()) {
            return Some(cmd.to_string());
        }
        if let Some(p) = raw
            .get("file_path")
            .or_else(|| raw.get("path"))
            .or_else(|| raw.get("filepath"))
            .and_then(|v| v.as_str())
        {
            let diff = edit_diff_suffix(raw);
            return Some(format!("{p}{diff}"));
        }
        if let Some(u) = raw.get("url").and_then(|v| v.as_str()) {
            return Some(u.to_string());
        }
        if let Some(pat) = raw
            .get("pattern")
            .or_else(|| raw.get("query"))
            .and_then(|v| v.as_str())
        {
            return Some(pat.to_string());
        }
    }
    locations.first().map(|l| l.path.display().to_string())
}

/// For Edit tools, compute a `+N -M` suffix from `old_string`/`new_string`.
fn edit_diff_suffix(raw: &serde_json::Value) -> String {
    let old = raw.get("old_string").and_then(|v| v.as_str());
    let new = raw.get("new_string").and_then(|v| v.as_str());
    if let (Some(old_s), Some(new_s)) = (old, new) {
        let del = if old_s.is_empty() {
            0
        } else {
            old_s.lines().count()
        };
        let add = if new_s.is_empty() {
            0
        } else {
            new_s.lines().count()
        };
        match (add, del) {
            (0, 0) => String::new(),
            (a, 0) => format!(" +{a}"),
            (0, d) => format!(" -{d}"),
            (a, d) => format!(" +{a} -{d}"),
        }
    } else {
        String::new()
    }
}

/// Map an initial `ToolCall`. The emitted `ToolCallStart` is *not* rendered on
/// its own — the bridge keeps it only to label the eventual completion. A
/// terminal status (e.g. an immediately-completed call) also yields the end.
fn tool_call_to_updates(tc: AcpToolCall) -> Vec<AgentUpdate> {
    let id = tc.tool_call_id.0.to_string();
    let kind = map_tool_kind(tc.kind);
    let label = tool_arg(tc.raw_input.as_ref(), &tc.locations).unwrap_or(tc.title);

    let mut out = vec![AgentUpdate::ToolCallStart {
        id: id.clone(),
        kind,
        label,
    }];
    push_status_end(&mut out, &id, tc.status);
    out
}

fn tool_call_update_to_updates(tcu: AcpToolCallUpdate) -> Vec<AgentUpdate> {
    let id = tcu.tool_call_id.0.to_string();
    let meta = tcu.meta;
    let fields = tcu.fields;
    let mut out = Vec::new();

    // Refresh the call's label whenever the update carries identifying info, so
    // the completion message can name the right file/command.
    let locations = fields.locations.clone().unwrap_or_default();
    if let Some(label) =
        tool_arg(fields.raw_input.as_ref(), &locations).or_else(|| fields.title.clone())
    {
        out.push(AgentUpdate::ToolCallStart {
            id: id.clone(),
            kind: fields.kind.map(map_tool_kind).unwrap_or(ToolKind::Other),
            label,
        });
    }

    // Textual output of the tool (its result on success, its error on failure).
    let output: Option<String> = fields.content.and_then(|content| {
        let text = content
            .into_iter()
            .filter_map(tool_call_content_text)
            .collect::<Vec<_>>()
            .join("\n");
        (!text.is_empty()).then_some(text)
    });
    if let Some(ref text) = output {
        out.push(AgentUpdate::ToolCallProgress {
            id: id.clone(),
            output_chunk: text.clone(),
        });
    }

    // A completion can reach us two ways:
    //  (1) `fields.status` = completed / failed — most tools, and all failures.
    //  (2) `status` absent, but `_meta.claudeCode.toolResponse` is populated —
    //      e.g. successful image reads carry the result only in `_meta`.
    match fields.status {
        Some(ToolCallStatus::Completed) => out.push(AgentUpdate::ToolCallEnd {
            id: id.clone(),
            ok: true,
            summary: None,
        }),
        // On failure, carry the tool's own output so the user sees the real
        // reason (e.g. git's stderr) instead of only "❌ <command>".
        Some(ToolCallStatus::Failed) => out.push(AgentUpdate::ToolCallEnd {
            id: id.clone(),
            ok: false,
            summary: output,
        }),
        _ if meta_has_tool_response(meta.as_ref()) => out.push(AgentUpdate::ToolCallEnd {
            id: id.clone(),
            ok: true,
            summary: None,
        }),
        _ => {}
    }

    out
}

/// True when the update's `_meta` carries `claudeCode.toolResponse`, which
/// claude-code-acp sets once a tool has produced its result.
fn meta_has_tool_response(meta: Option<&serde_json::Map<String, serde_json::Value>>) -> bool {
    meta.and_then(|m| m.get("claudeCode"))
        .and_then(|c| c.get("toolResponse"))
        .is_some()
}

/// Append a `ToolCallEnd` when the tool reaches a terminal status, marking it as
/// succeeded (✅) or failed (❌). The user sees one completion message per tool;
/// non-terminal updates (pending / in-progress) produce nothing.
fn push_status_end(out: &mut Vec<AgentUpdate>, id: &str, status: ToolCallStatus) {
    let ok = match status {
        ToolCallStatus::Completed => true,
        ToolCallStatus::Failed => false,
        _ => return,
    };
    out.push(AgentUpdate::ToolCallEnd {
        id: id.to_string(),
        ok,
        summary: None,
    });
}

fn tool_call_content_text(c: ToolCallContent) -> Option<String> {
    let json = serde_json::to_value(&c).ok()?;
    if let Some(t) = json.get("text").and_then(|v| v.as_str()) {
        return Some(t.to_string());
    }
    if let Some(content) = json.get("content") {
        if let Some(t) = content.get("text").and_then(|v| v.as_str()) {
            return Some(t.to_string());
        }
    }
    None
}

fn plan_to_update(plan: Plan) -> Option<AgentUpdate> {
    let json = serde_json::to_value(&plan).ok()?;
    let entries = json.get("entries")?.as_array()?;
    let steps: Vec<String> = entries
        .iter()
        .filter_map(|e| {
            let content = e.get("content").and_then(|v| v.as_str())?;
            let status = e
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("pending");
            let text = match status {
                "completed" => format!("~~{}~~ ✅", content),
                "in_progress" => format!("{} ⏳", content),
                _ => content.to_string(),
            };
            Some(text)
        })
        .collect();
    if steps.is_empty() {
        None
    } else {
        Some(AgentUpdate::Plan { steps })
    }
}

fn mode_changed(u: CurrentModeUpdate) -> AgentUpdate {
    AgentUpdate::ModeChanged {
        mode_id: u.current_mode_id.0.to_string(),
    }
}

fn session_info_to_update(u: SessionInfoUpdate) -> Option<AgentUpdate> {
    // Only emit an update if the title was actually set to a non-empty string.
    let title = u.title.take()?;
    if title.is_empty() {
        return None;
    }
    Some(AgentUpdate::SessionInfo { title })
}

pub fn map_tool_kind(k: AcpToolKind) -> ToolKind {
    match k {
        AcpToolKind::Read => ToolKind::FileRead,
        AcpToolKind::Edit => ToolKind::FileEdit,
        AcpToolKind::Delete => ToolKind::FileEdit,
        AcpToolKind::Move => ToolKind::FileEdit,
        AcpToolKind::Search => ToolKind::Search,
        AcpToolKind::Execute => ToolKind::Shell,
        AcpToolKind::Think => ToolKind::Other,
        AcpToolKind::Fetch => ToolKind::Web,
        AcpToolKind::SwitchMode => ToolKind::Other,
        AcpToolKind::Other => ToolKind::Other,
        _ => ToolKind::Other,
    }
}
