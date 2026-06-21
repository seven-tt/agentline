//! kimi-specific ACP tool-call normalization.
//!
//! kimi's ACP server doesn't follow the spec for tool calls: a Bash permission
//! request arrives with `kind: None`, `title: "Bash"`, and the arguments as a
//! JSON blob inside `content` (`{"command": "...", "timeout": 120}`) rather than
//! in `raw_input`. Left alone the generic parser classifies it as `Other`, so
//! the permission policy can't treat it as a shell command.
//!
//! Additionally, Kimi may send a ToolCall notification (with content) *before*
//! the permission request (which may lack content). The `observe`/`enrich_permission`
//! hooks cache ToolCall data so the permission label is resolved correctly.

use agentline_agent_acp::{ToolCallParser, ToolCallUpdate};
use agentline_bridge::ToolKind;
use agentline_bridge::format::tool_label;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Default)]
pub struct KimiToolCallParser {
    cache: Mutex<HashMap<String, serde_json::Value>>,
}

impl KimiToolCallParser {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ToolCallParser for KimiToolCallParser {
    fn refine_kind(&self, tc: &ToolCallUpdate) -> Option<ToolKind> {
        if content_command(tc).is_some() {
            return Some(ToolKind::Shell);
        }
        title_shell(tc)
    }

    fn refine_what(&self, tc: &ToolCallUpdate) -> Option<String> {
        content_command(tc).map(|cmd| tool_label(ToolKind::Shell, &cmd))
    }

    fn observe(&self, tc: &ToolCallUpdate) {
        if let Ok(v) = serde_json::to_value(tc)
            && v.get("content")
                .and_then(|c| c.as_array())
                .is_some_and(|a| !a.is_empty())
        {
            let id = tc.tool_call_id.0.to_string();
            if let Ok(mut cache) = self.cache.lock() {
                cache.insert(id, v);
            }
        }
    }

    fn enrich_permission(&self, tc: ToolCallUpdate) -> ToolCallUpdate {
        let id = tc.tool_call_id.0.to_string();
        let cached = self.cache.lock().ok().and_then(|c| c.get(&id).cloned());
        if let Some(cached_json) = cached
            && let Ok(mut enriched) = serde_json::from_value::<ToolCallUpdate>(cached_json)
        {
            if enriched.fields.title.is_none() {
                enriched.fields.title = tc.fields.title;
            }
            return enriched;
        }
        tc
    }
}

/// Pull a shell command out of `content` (a JSON text blob).
fn content_command(tc: &ToolCallUpdate) -> Option<String> {
    let v = serde_json::to_value(tc).ok()?;
    let content = v.get("content")?.as_array()?;
    for block in content {
        let text = block
            .get("content")
            .and_then(|c| c.get("text"))
            .and_then(|t| t.as_str())
            .or_else(|| block.get("text").and_then(|t| t.as_str()));
        if let Some(text) = text
            && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text)
            && let Some(cmd) = parsed.get("command").and_then(|x| x.as_str())
            && !cmd.is_empty()
        {
            return Some(cmd.to_string());
        }
    }
    None
}

/// kimi tags a Bash call with `title: "Bash"` (and no usable kind/raw_input).
fn title_shell(tc: &ToolCallUpdate) -> Option<ToolKind> {
    let t = tc.fields.title.as_ref()?.trim().to_lowercase();
    matches!(
        t.as_str(),
        "bash" | "sh" | "zsh" | "shell" | "execute" | "exec" | "run" | "command" | "terminal"
    )
    .then_some(ToolKind::Shell)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tc(json: serde_json::Value) -> ToolCallUpdate {
        serde_json::from_value(json).expect("valid ToolCallUpdate json")
    }

    #[test]
    fn title_bash_is_shell() {
        let t = tc(serde_json::json!({ "toolCallId": "t1", "title": "Bash" }));
        assert_eq!(
            KimiToolCallParser::new().refine_kind(&t),
            Some(ToolKind::Shell)
        );
    }

    #[test]
    fn content_command_is_shell_and_labeled() {
        let t = tc(serde_json::json!({
            "toolCallId": "t1",
            "content": [{
                "type": "content",
                "content": { "type": "text", "text": "{\"command\": \"git clone x\"}" }
            }]
        }));
        let parser = KimiToolCallParser::new();
        assert_eq!(parser.refine_kind(&t), Some(ToolKind::Shell));
        let what = parser.refine_what(&t).expect("some label");
        assert!(what.contains("git clone x"), "what was: {what}");
    }

    #[test]
    fn unrelated_title_is_none() {
        let t = tc(serde_json::json!({ "toolCallId": "t1", "title": "Read file" }));
        let parser = KimiToolCallParser::new();
        assert_eq!(parser.refine_kind(&t), None);
        assert_eq!(parser.refine_what(&t), None);
    }

    #[test]
    fn observe_then_enrich_permission() {
        let parser = KimiToolCallParser::new();
        let notification_tc = tc(serde_json::json!({
            "toolCallId": "t1",
            "title": "Bash",
            "content": [{
                "type": "content",
                "content": { "type": "text", "text": "{\"command\": \"ls -la\"}" }
            }]
        }));
        parser.observe(&notification_tc);

        let perm_tc = tc(serde_json::json!({ "toolCallId": "t1", "title": "Bash" }));
        let enriched = parser.enrich_permission(perm_tc);
        let what = parser
            .refine_what(&enriched)
            .expect("should resolve command");
        assert!(what.contains("ls -la"), "what was: {what}");
    }
}
