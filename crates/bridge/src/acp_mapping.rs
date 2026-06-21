//! Bidirectional mapping between ACP `SessionUpdate` and agentline `AgentUpdate`.
//!
//! With `AgentUpdate::Session(SessionUpdate)` the mapping is trivial — wrap and
//! unwrap.  This module also hosts helpers for extracting agentline-specific
//! presentation fields (tool labels, plan rendering) from the ACP types.

use crate::{AgentUpdate, ToolKind};
use agent_client_protocol::{
    SessionUpdate, ToolCallStatus, ToolCallUpdate as AcpToolCallUpdate, ToolKind as AcpToolKind,
};

// ── Wrap / unwrap ───────────────────────────────────────────────────────

pub fn map_session_update(u: SessionUpdate) -> Vec<AgentUpdate> {
    vec![AgentUpdate::Session(u)]
}

pub fn agent_update_to_session_update(u: &AgentUpdate) -> Option<SessionUpdate> {
    match u {
        AgentUpdate::Session(su) => Some(su.clone()),
        _ => None,
    }
}

// ── Tool-kind mapping ───────────────────────────────────────────────────

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

pub fn reverse_tool_kind(k: ToolKind) -> AcpToolKind {
    match k {
        ToolKind::Shell => AcpToolKind::Execute,
        ToolKind::FileRead => AcpToolKind::Read,
        ToolKind::FileEdit => AcpToolKind::Edit,
        ToolKind::FileWrite => AcpToolKind::Edit,
        ToolKind::Search => AcpToolKind::Search,
        ToolKind::Web => AcpToolKind::Fetch,
        ToolKind::Mcp => AcpToolKind::Other,
        ToolKind::Other => AcpToolKind::Other,
    }
}

// ── Tool label extraction ───────────────────────────────────────────────

pub fn tool_arg(
    raw_input: Option<&serde_json::Value>,
    locations: &[agent_client_protocol::ToolCallLocation],
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

// ── Tool-call status helpers ────────────────────────────────────────────

pub fn tool_call_update_status(tcu: &AcpToolCallUpdate) -> Option<(bool, Option<String>)> {
    match tcu.fields.status {
        Some(ToolCallStatus::Completed) => Some((true, None)),
        Some(ToolCallStatus::Failed) => {
            let summary = tcu.fields.content.as_ref().and_then(|content| {
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
                (!text.is_empty()).then_some(text)
            });
            Some((false, summary))
        }
        _ => {
            if meta_has_tool_response(tcu.meta.as_ref()) {
                Some((true, None))
            } else {
                None
            }
        }
    }
}

fn meta_has_tool_response(meta: Option<&serde_json::Map<String, serde_json::Value>>) -> bool {
    meta.and_then(|m| m.get("claudeCode"))
        .and_then(|c| c.get("toolResponse"))
        .is_some()
}

// ── Plan rendering ──────────────────────────────────────────────────────

pub fn plan_to_steps(plan: &agent_client_protocol::Plan) -> Vec<String> {
    let json = match serde_json::to_value(plan) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let entries = match json.get("entries").and_then(|e| e.as_array()) {
        Some(a) => a,
        None => return Vec::new(),
    };
    entries
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
        .collect()
}
