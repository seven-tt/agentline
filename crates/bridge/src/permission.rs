use crate::types::{AgentSessionId, PeerRef, ToolKind};
use tokio::sync::oneshot;

pub use agentline_permission::{
    AutoApprove, AutoApproveReason, PermissionDanger, PermissionDecision, PermissionPolicy,
    PermissionResponse,
};

pub use agentline_permission::{extract_shell_cmd, extract_shell_grants, shell_matches_grant};

#[derive(Debug)]
pub struct PendingPermissionRequest {
    pub agent_session_id: AgentSessionId,
    pub request_id: String,
    pub responder: oneshot::Sender<PermissionResponse>,
    pub peer: PeerRef,
    pub tool_kind: ToolKind,
    pub what: String,
}
