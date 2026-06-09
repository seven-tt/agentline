use crate::types::{PeerRef, SessionId, ToolKind};
use std::collections::HashSet;
use tokio::sync::oneshot;

// ── Types migrated from types.rs / state.rs ──────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDanger {
    Low,
    Medium,
    High,
}

#[derive(Debug)]
pub struct PendingPerm {
    pub session_id: SessionId,
    pub request_id: String,
    pub responder: oneshot::Sender<PermResponse>,
    pub peer: PeerRef,
    pub tool_kind: ToolKind,
    /// The raw "what" label from the permission request, used to extract
    /// the shell command prefix for fine-grained session grants.
    pub what: String,
}

#[derive(Debug, Clone, Copy)]
pub enum PermResponse {
    Once,
    Session,
    Deny,
}

#[derive(Clone, Copy, Debug)]
pub enum AutoApprove {
    No,
    Session,
    Yolo,
}

// ── Shell blacklist ──────────────────────────────────────────────────

const SHELL_BLACKLIST: &[&str] = &[
    "rm", "mv", "dd", "shred", "chmod", "chown", "kill", "pkill", "killall", "reboot", "shutdown",
    "halt", "curl", "wget", "nc", "ssh",
];

fn is_blacklisted(cmd: &str) -> bool {
    SHELL_BLACKLIST.contains(&cmd)
}

// ── PermissionPolicy ─────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PermissionPolicy {
    yolo: bool,
    granted_kinds: HashSet<ToolKind>,
    granted_shell_cmds: HashSet<String>,
}

pub enum PermissionDecision {
    AutoApprove(AutoApproveReason),
    Ask,
}

#[derive(Debug, Clone, Copy)]
pub enum AutoApproveReason {
    Yolo,
    SessionGrant,
}

impl Default for PermissionPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl PermissionPolicy {
    pub fn new() -> Self {
        Self {
            yolo: false,
            granted_kinds: HashSet::new(),
            granted_shell_cmds: HashSet::new(),
        }
    }

    pub fn is_yolo(&self) -> bool {
        self.yolo
    }

    pub fn set_yolo(&mut self, on: bool) {
        self.yolo = on;
    }

    pub fn clear_grants(&mut self) {
        self.granted_kinds.clear();
        self.granted_shell_cmds.clear();
    }

    pub fn granted_kinds(&self) -> &HashSet<ToolKind> {
        &self.granted_kinds
    }

    pub fn granted_shell_cmds(&self) -> &HashSet<String> {
        &self.granted_shell_cmds
    }

    /// Decide whether a permission request should be auto-approved or
    /// needs user confirmation.
    ///
    /// `what` is the tool label string (e.g. `"🔧 Shell（ls -la）"`),
    /// used to extract the shell command prefix for fine-grained matching.
    pub fn evaluate(&self, tool_kind: ToolKind, what: &str) -> PermissionDecision {
        if self.yolo {
            return PermissionDecision::AutoApprove(AutoApproveReason::Yolo);
        }

        match tool_kind {
            ToolKind::Shell => {
                if let Some(cmd) = extract_shell_cmd(what)
                    && !is_blacklisted(cmd)
                    && self.granted_shell_cmds.contains(cmd)
                {
                    return PermissionDecision::AutoApprove(AutoApproveReason::SessionGrant);
                }
                PermissionDecision::Ask
            }
            ToolKind::Other => PermissionDecision::Ask,
            _ => {
                if self.granted_kinds.contains(&tool_kind) {
                    PermissionDecision::AutoApprove(AutoApproveReason::SessionGrant)
                } else {
                    PermissionDecision::Ask
                }
            }
        }
    }

    /// Record a session-level grant after the user responds.
    pub fn apply_response(&mut self, tool_kind: ToolKind, resp: PermResponse, what: &str) {
        if !matches!(resp, PermResponse::Session) {
            return;
        }
        match tool_kind {
            ToolKind::Shell => {
                if let Some(cmd) = extract_shell_cmd(what)
                    && !is_blacklisted(cmd)
                {
                    self.granted_shell_cmds.insert(cmd.to_string());
                }
            }
            ToolKind::Other => {}
            _ => {
                self.granted_kinds.insert(tool_kind);
            }
        }
    }

    /// Downgrade the user's response when session-granting is not
    /// applicable, so the UI feedback matches reality.
    pub fn effective_response(
        &self,
        tool_kind: ToolKind,
        resp: PermResponse,
        what: &str,
    ) -> PermResponse {
        if !matches!(resp, PermResponse::Session) {
            return resp;
        }
        match tool_kind {
            ToolKind::Shell => {
                if let Some(cmd) = extract_shell_cmd(what)
                    && is_blacklisted(cmd)
                {
                    return PermResponse::Once;
                }
                resp
            }
            ToolKind::Other => PermResponse::Once,
            _ => resp,
        }
    }

    /// Summary of granted permissions for display in `/sessions`.
    pub fn grant_summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.granted_kinds.is_empty() {
            let mut names: Vec<&str> = self.granted_kinds.iter().map(kind_label).collect();
            names.sort_unstable();
            parts.extend(names.into_iter().map(String::from));
        }
        if !self.granted_shell_cmds.is_empty() {
            let mut cmds: Vec<&str> = self.granted_shell_cmds.iter().map(|s| s.as_str()).collect();
            cmds.sort_unstable();
            for cmd in cmds {
                parts.push(format!("Shell({cmd})"));
            }
        }
        if parts.is_empty() {
            "无".to_string()
        } else {
            parts.join("、")
        }
    }
}

/// Extract the shell command name (first word) from a tool label string.
///
/// The `what` format is `"🔧 Shell（<command>）"` as produced by
/// `format::tool_label(ToolKind::Shell, cmd)`.
pub fn extract_shell_cmd(what: &str) -> Option<&str> {
    let inner = what.split('（').nth(1)?.strip_suffix('）')?;
    inner.split_whitespace().next()
}

fn kind_label(k: &ToolKind) -> &'static str {
    match k {
        ToolKind::Shell => "Shell",
        ToolKind::FileRead => "读文件",
        ToolKind::FileEdit => "改文件",
        ToolKind::FileWrite => "写文件",
        ToolKind::Search => "搜索",
        ToolKind::Web => "Web",
        ToolKind::Other => "其他",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell_what(cmd: &str) -> String {
        format!("🔧 Shell（{}）", cmd)
    }

    fn file_what(path: &str) -> String {
        format!("📖 FileRead（{}）", path)
    }

    #[test]
    fn extract_shell_cmd_works() {
        assert_eq!(extract_shell_cmd(&shell_what("ls -la /tmp")), Some("ls"));
        assert_eq!(extract_shell_cmd(&shell_what("rm -rf /")), Some("rm"));
        assert_eq!(extract_shell_cmd(&shell_what("git status")), Some("git"));
        assert_eq!(extract_shell_cmd(&file_what("foo.rs")), Some("foo.rs"));
    }

    #[test]
    fn yolo_approves_everything() {
        let mut p = PermissionPolicy::new();
        p.set_yolo(true);
        assert!(matches!(
            p.evaluate(ToolKind::Shell, &shell_what("rm -rf /")),
            PermissionDecision::AutoApprove(AutoApproveReason::Yolo)
        ));
        assert!(matches!(
            p.evaluate(ToolKind::Other, ""),
            PermissionDecision::AutoApprove(AutoApproveReason::Yolo)
        ));
    }

    #[test]
    fn shell_session_grant_by_prefix() {
        let mut p = PermissionPolicy::new();
        let what = shell_what("ls -la /tmp");
        p.apply_response(ToolKind::Shell, PermResponse::Session, &what);

        // ls is granted
        assert!(matches!(
            p.evaluate(ToolKind::Shell, &shell_what("ls -R .")),
            PermissionDecision::AutoApprove(AutoApproveReason::SessionGrant)
        ));
        // git is not
        assert!(matches!(
            p.evaluate(ToolKind::Shell, &shell_what("git status")),
            PermissionDecision::Ask
        ));
    }

    #[test]
    fn shell_blacklist_never_granted() {
        let mut p = PermissionPolicy::new();
        let what = shell_what("rm -rf /tmp/x");
        p.apply_response(ToolKind::Shell, PermResponse::Session, &what);

        // rm is blacklisted — not stored
        assert!(p.granted_shell_cmds.is_empty());
        assert!(matches!(
            p.evaluate(ToolKind::Shell, &shell_what("rm foo")),
            PermissionDecision::Ask
        ));
    }

    #[test]
    fn shell_blacklist_effective_response_downgrades() {
        let p = PermissionPolicy::new();
        assert!(matches!(
            p.effective_response(ToolKind::Shell, PermResponse::Session, &shell_what("rm x")),
            PermResponse::Once
        ));
        // Non-blacklisted shell keeps Session
        assert!(matches!(
            p.effective_response(ToolKind::Shell, PermResponse::Session, &shell_what("ls x")),
            PermResponse::Session
        ));
    }

    #[test]
    fn non_shell_session_grant_by_kind() {
        let mut p = PermissionPolicy::new();
        p.apply_response(ToolKind::FileRead, PermResponse::Session, "");

        assert!(matches!(
            p.evaluate(ToolKind::FileRead, &file_what("any_file.rs")),
            PermissionDecision::AutoApprove(AutoApproveReason::SessionGrant)
        ));
        // FileEdit is separate
        assert!(matches!(
            p.evaluate(ToolKind::FileEdit, ""),
            PermissionDecision::Ask
        ));
    }

    #[test]
    fn other_never_session_granted() {
        let mut p = PermissionPolicy::new();
        p.apply_response(ToolKind::Other, PermResponse::Session, "");

        assert!(matches!(
            p.evaluate(ToolKind::Other, ""),
            PermissionDecision::Ask
        ));
    }

    #[test]
    fn other_effective_response_downgrades() {
        let p = PermissionPolicy::new();
        assert!(matches!(
            p.effective_response(ToolKind::Other, PermResponse::Session, ""),
            PermResponse::Once
        ));
    }

    #[test]
    fn clear_grants_resets_all() {
        let mut p = PermissionPolicy::new();
        p.apply_response(ToolKind::FileRead, PermResponse::Session, "");
        p.apply_response(ToolKind::Shell, PermResponse::Session, &shell_what("ls x"));
        assert!(!p.granted_kinds.is_empty());
        assert!(!p.granted_shell_cmds.is_empty());

        p.clear_grants();
        assert!(p.granted_kinds.is_empty());
        assert!(p.granted_shell_cmds.is_empty());
    }

    #[test]
    fn grant_summary_shows_all() {
        let mut p = PermissionPolicy::new();
        p.apply_response(ToolKind::FileRead, PermResponse::Session, "");
        p.apply_response(ToolKind::Shell, PermResponse::Session, &shell_what("ls x"));
        let s = p.grant_summary();
        assert!(s.contains("读文件"));
        assert!(s.contains("Shell(ls)"));
    }
}
