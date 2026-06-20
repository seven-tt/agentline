mod policy;
mod shell;
mod tool_kind;

pub use policy::{
    AutoApprove, AutoApproveReason, PermissionDecision, PermissionPolicy, PermissionResponse,
};
pub use shell::{extract_shell_cmd, extract_shell_grants, shell_matches_grant};
pub use tool_kind::ToolKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDanger {
    Low,
    Medium,
    High,
}
