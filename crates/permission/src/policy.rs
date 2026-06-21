use crate::shell::{extract_shell_grants, is_command_auto_approvable, shell_matches_grant};
use crate::tool_kind::ToolKind;
use std::collections::HashSet;
use std::path::Path;

#[derive(Debug, Clone, Copy)]
pub enum PermissionResponse {
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

pub enum PermissionDecision {
    AutoApprove(AutoApproveReason),
    Ask,
}

#[derive(Debug, Clone, Copy)]
pub enum AutoApproveReason {
    Yolo,
    SessionGrant,
    SafeInCwd,
}

#[derive(Debug, Clone)]
pub struct PermissionPolicy {
    yolo: bool,
    granted_kinds: HashSet<ToolKind>,
    granted_shell_cmds: HashSet<String>,
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
    /// `what` is the tool label string (e.g. `"🔧 Shell（ls -la）"`).
    /// `cwd` is the session's current working directory for safe-in-cwd checks.
    pub fn evaluate(&self, tool_kind: ToolKind, what: &str, cwd: &Path) -> PermissionDecision {
        if self.yolo {
            return PermissionDecision::AutoApprove(AutoApproveReason::Yolo);
        }

        match tool_kind {
            ToolKind::Shell => {
                // Tier 0: Auto-approve safe commands within CWD
                if is_command_auto_approvable(what, cwd) {
                    return PermissionDecision::AutoApprove(AutoApproveReason::SafeInCwd);
                }
                // Tier 1-3: Check session grants
                if !self.granted_shell_cmds.is_empty()
                    && shell_matches_grant(what, &self.granted_shell_cmds)
                {
                    return PermissionDecision::AutoApprove(AutoApproveReason::SessionGrant);
                }
                PermissionDecision::Ask
            }
            ToolKind::Mcp => {
                if is_mcp_auto_approvable(what) {
                    return PermissionDecision::AutoApprove(AutoApproveReason::SafeInCwd);
                }
                if self.granted_kinds.contains(&ToolKind::Mcp) {
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
    pub fn apply_response(&mut self, tool_kind: ToolKind, resp: PermissionResponse, what: &str) {
        if !matches!(resp, PermissionResponse::Session) {
            return;
        }
        match tool_kind {
            ToolKind::Shell => {
                for grant in extract_shell_grants(what) {
                    self.granted_shell_cmds.insert(grant);
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
        resp: PermissionResponse,
        what: &str,
    ) -> PermissionResponse {
        if !matches!(resp, PermissionResponse::Session) {
            return resp;
        }
        match tool_kind {
            ToolKind::Shell => {
                let grants = extract_shell_grants(what);
                if grants.is_empty() {
                    return PermissionResponse::Once;
                }
                resp
            }
            ToolKind::Other => PermissionResponse::Once,
            _ => resp,
        }
    }

    pub fn resolve_and_apply(
        &mut self,
        tool_kind: ToolKind,
        response: PermissionResponse,
        what: &str,
    ) -> PermissionResponse {
        let effective = self.effective_response(tool_kind, response, what);
        self.apply_response(tool_kind, effective, what);
        effective
    }

    /// Summary of granted permissions for display in `/sessions`.
    pub fn grant_summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.granted_kinds.is_empty() {
            let mut names: Vec<&str> = self.granted_kinds.iter().map(|k| k.name()).collect();
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
            "-".to_string()
        } else {
            parts.join(", ")
        }
    }
}

fn is_mcp_auto_approvable(what: &str) -> bool {
    what.contains("mcp__agentline__")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cwd() -> PathBuf {
        PathBuf::from("/Users/seven/project")
    }

    fn shell_what(cmd: &str) -> String {
        format!("🔧 Shell({})", cmd)
    }

    fn file_what(path: &str) -> String {
        format!("📖 FileRead({})", path)
    }

    #[test]
    fn yolo_approves_everything() {
        let mut p = PermissionPolicy::new();
        p.set_yolo(true);
        assert!(matches!(
            p.evaluate(ToolKind::Shell, &shell_what("rm -rf /"), &cwd()),
            PermissionDecision::AutoApprove(AutoApproveReason::Yolo)
        ));
        assert!(matches!(
            p.evaluate(ToolKind::Other, "", &cwd()),
            PermissionDecision::AutoApprove(AutoApproveReason::Yolo)
        ));
    }

    #[test]
    fn safe_commands_auto_approved_in_cwd() {
        let p = PermissionPolicy::new();
        assert!(matches!(
            p.evaluate(ToolKind::Shell, &shell_what("git status"), &cwd()),
            PermissionDecision::AutoApprove(AutoApproveReason::SafeInCwd)
        ));
        assert!(matches!(
            p.evaluate(ToolKind::Shell, &shell_what("ls -la"), &cwd()),
            PermissionDecision::AutoApprove(AutoApproveReason::SafeInCwd)
        ));
        assert!(matches!(
            p.evaluate(ToolKind::Shell, &shell_what("cargo build"), &cwd()),
            PermissionDecision::AutoApprove(AutoApproveReason::SafeInCwd)
        ));
    }

    #[test]
    fn safe_commands_outside_cwd_not_auto_approved() {
        let p = PermissionPolicy::new();
        assert!(matches!(
            p.evaluate(ToolKind::Shell, &shell_what("cd /tmp"), &cwd()),
            PermissionDecision::Ask
        ));
        assert!(matches!(
            p.evaluate(ToolKind::Shell, &shell_what("cat /etc/passwd"), &cwd()),
            PermissionDecision::Ask
        ));
    }

    #[test]
    fn unknown_commands_require_ask() {
        let p = PermissionPolicy::new();
        assert!(matches!(
            p.evaluate(ToolKind::Shell, &shell_what("sed -i 's/a/b/' file"), &cwd()),
            PermissionDecision::Ask
        ));
    }

    #[test]
    fn session_grant_first_word_for_safe() {
        let mut p = PermissionPolicy::new();
        let what = shell_what("git status");
        p.apply_response(ToolKind::Shell, PermissionResponse::Session, &what);

        // git is already auto-approved in cwd, but also granted for outside-cwd
        assert!(matches!(
            p.evaluate(ToolKind::Shell, &shell_what("git push origin main"), &cwd()),
            PermissionDecision::AutoApprove(_)
        ));
    }

    #[test]
    fn session_grant_full_match_for_path_cmd() {
        let mut p = PermissionPolicy::new();
        let what = shell_what("cd /tmp");
        p.apply_response(ToolKind::Shell, PermissionResponse::Session, &what);

        assert!(matches!(
            p.evaluate(ToolKind::Shell, &shell_what("cd /tmp"), &cwd()),
            PermissionDecision::AutoApprove(AutoApproveReason::SessionGrant)
        ));
        assert!(matches!(
            p.evaluate(ToolKind::Shell, &shell_what("cd /etc"), &cwd()),
            PermissionDecision::Ask
        ));
    }

    #[test]
    fn session_grant_full_match_for_unknown() {
        let mut p = PermissionPolicy::new();
        let what = shell_what("sed -i 's/a/b/' /tmp/x");
        p.apply_response(ToolKind::Shell, PermissionResponse::Session, &what);

        assert!(matches!(
            p.evaluate(
                ToolKind::Shell,
                &shell_what("sed -i 's/a/b/' /tmp/x"),
                &cwd()
            ),
            PermissionDecision::AutoApprove(AutoApproveReason::SessionGrant)
        ));
        assert!(matches!(
            p.evaluate(
                ToolKind::Shell,
                &shell_what("sed -i 's/c/d/' /tmp/y"),
                &cwd()
            ),
            PermissionDecision::Ask
        ));
    }

    #[test]
    fn relative_path_not_granted() {
        let mut p = PermissionPolicy::new();
        let what = shell_what("cd agentline && ls -la");
        p.apply_response(ToolKind::Shell, PermissionResponse::Session, &what);

        // cd relative is not stored, only ls is
        assert!(p.granted_shell_cmds.contains("ls"));
        assert!(!p.granted_shell_cmds.iter().any(|s| s.starts_with("cd")));
    }

    #[test]
    fn blacklist_never_granted() {
        let mut p = PermissionPolicy::new();
        let what = shell_what("rm -rf /tmp/x");
        p.apply_response(ToolKind::Shell, PermissionResponse::Session, &what);
        assert!(p.granted_shell_cmds.is_empty());
    }

    #[test]
    fn effective_response_downgrades() {
        let p = PermissionPolicy::new();
        assert!(matches!(
            p.effective_response(
                ToolKind::Shell,
                PermissionResponse::Session,
                &shell_what("rm x")
            ),
            PermissionResponse::Once
        ));
        assert!(matches!(
            p.effective_response(
                ToolKind::Shell,
                PermissionResponse::Session,
                &shell_what("cd relative")
            ),
            PermissionResponse::Once
        ));
        assert!(matches!(
            p.effective_response(
                ToolKind::Shell,
                PermissionResponse::Session,
                &shell_what("git status")
            ),
            PermissionResponse::Session
        ));
    }

    #[test]
    fn non_shell_session_grant_by_kind() {
        let mut p = PermissionPolicy::new();
        p.apply_response(ToolKind::FileRead, PermissionResponse::Session, "");

        assert!(matches!(
            p.evaluate(ToolKind::FileRead, &file_what("any_file.rs"), &cwd()),
            PermissionDecision::AutoApprove(AutoApproveReason::SessionGrant)
        ));
        assert!(matches!(
            p.evaluate(ToolKind::FileEdit, "", &cwd()),
            PermissionDecision::Ask
        ));
    }

    #[test]
    fn other_never_session_granted() {
        let mut p = PermissionPolicy::new();
        p.apply_response(ToolKind::Other, PermissionResponse::Session, "");
        assert!(matches!(
            p.evaluate(ToolKind::Other, "", &cwd()),
            PermissionDecision::Ask
        ));
    }

    fn mcp_what(tool: &str) -> String {
        format!("🔌 Mcp({})", tool)
    }

    #[test]
    fn mcp_agentline_auto_approved() {
        let p = PermissionPolicy::new();
        assert!(matches!(
            p.evaluate(
                ToolKind::Mcp,
                &mcp_what("mcp__agentline__list_projects"),
                &cwd()
            ),
            PermissionDecision::AutoApprove(AutoApproveReason::SafeInCwd)
        ));
    }

    #[test]
    fn mcp_third_party_requires_ask() {
        let p = PermissionPolicy::new();
        assert!(matches!(
            p.evaluate(ToolKind::Mcp, &mcp_what("mcp__github__search"), &cwd()),
            PermissionDecision::Ask
        ));
    }

    #[test]
    fn mcp_session_grant() {
        let mut p = PermissionPolicy::new();
        p.apply_response(
            ToolKind::Mcp,
            PermissionResponse::Session,
            &mcp_what("mcp__github__search"),
        );
        assert!(matches!(
            p.evaluate(ToolKind::Mcp, &mcp_what("mcp__github__list_repos"), &cwd()),
            PermissionDecision::AutoApprove(AutoApproveReason::SessionGrant)
        ));
    }

    #[test]
    fn clear_grants_resets_all() {
        let mut p = PermissionPolicy::new();
        p.apply_response(ToolKind::FileRead, PermissionResponse::Session, "");
        p.apply_response(
            ToolKind::Shell,
            PermissionResponse::Session,
            &shell_what("cd /tmp"),
        );
        assert!(!p.granted_kinds.is_empty());
        assert!(!p.granted_shell_cmds.is_empty());

        p.clear_grants();
        assert!(p.granted_kinds.is_empty());
        assert!(p.granted_shell_cmds.is_empty());
    }

    #[test]
    fn grant_summary_shows_all() {
        let mut p = PermissionPolicy::new();
        p.apply_response(ToolKind::FileRead, PermissionResponse::Session, "");
        p.apply_response(
            ToolKind::Shell,
            PermissionResponse::Session,
            &shell_what("cd /tmp"),
        );
        let s = p.grant_summary();
        assert!(s.contains("FileRead"));
        assert!(s.contains("Shell(cd /tmp)"));
    }
}
