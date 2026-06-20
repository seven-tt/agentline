use crate::permission::PendingPermissionRequest;
use crate::session::SessionManager;
use crate::types::{AgentSessionId, ContentBlock, PeerRef, SessionId};
use agent_client_protocol::ElicitationSchema;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

/// In-memory state owned by the bridge.
#[derive(Debug)]
pub struct BridgeState {
    pub cwd: PathBuf,
    pub sessions: SessionManager,
    pub pending_perms: HashMap<SessionId, PendingPermissionRequest>,
    pub pending_elicits: HashMap<SessionId, PendingElicit>,
    /// When `/yolo` is sent before a session exists, the intent is stored here
    /// and applied to the next session created.  Cleared by `/new`.
    pub pending_yolo: bool,
    /// Prompts that arrived while another prompt_task was still running.
    /// Dequeued automatically when the current turn finishes.
    pub pending_prompts: VecDeque<(SessionId, Vec<ContentBlock>)>,
    /// Current agent backend name (mutable via `/agent`).
    pub agent_name: String,
    /// Which session's prompt is currently executing, if any.
    pub running_session: Option<SessionId>,
}

impl BridgeState {
    pub fn new(default_cwd: PathBuf, agent_name: String) -> Self {
        Self {
            cwd: default_cwd,
            sessions: SessionManager::new(),
            pending_perms: HashMap::new(),
            pending_elicits: HashMap::new(),
            pending_yolo: false,
            pending_prompts: VecDeque::new(),
            agent_name,
            running_session: None,
        }
    }
}

#[derive(Debug)]
pub struct PendingElicit {
    pub agent_session_id: AgentSessionId,
    pub elicit_id: String,
    pub peer: PeerRef,
    pub schema: Option<ElicitationSchema>,
}
