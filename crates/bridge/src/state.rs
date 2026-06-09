use crate::permission::{PendingPerm, PermissionPolicy};
use crate::types::{ElicitField, PeerRef, SessionId};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Instant, SystemTime};

/// In-memory state owned by the bridge.
#[derive(Debug)]
pub struct BridgeState {
    pub cwd: PathBuf,
    pub current: Option<ActiveSession>,
    pub pending_perm: Option<PendingPerm>,
    pub pending_elicit: Option<PendingElicit>,
    /// Waiting for the user to pick a numbered option (from /stop or /cd without args).
    pub pending_selection: Option<PendingSelection>,
    /// Auto-incremented; used to assign `ActiveSession.short_id`.
    pub session_counter: u32,
    /// When `/yolo` is sent before a session exists, the intent is stored here
    /// and applied to the next session created.  Cleared by `/new`.
    pub pending_yolo: bool,
    /// True once the project list has been injected into the first prompt of
    /// the current session. Reset by `/new`.
    pub project_context_sent: bool,
    /// Prompts that arrived while another prompt_task was still running.
    /// Dequeued automatically when the current turn finishes.
    pub pending_prompts: VecDeque<(PeerRef, String)>,
}

impl BridgeState {
    pub fn new(default_cwd: PathBuf) -> Self {
        Self {
            cwd: default_cwd,
            current: None,
            pending_perm: None,
            pending_elicit: None,
            pending_selection: None,
            session_counter: 0,
            pending_yolo: false,
            project_context_sent: false,
            pending_prompts: VecDeque::new(),
        }
    }

    pub fn next_short_id(&mut self) -> u32 {
        self.session_counter += 1;
        self.session_counter
    }
}

#[derive(Debug)]
pub struct ActiveSession {
    pub peer: PeerRef,
    pub cwd: PathBuf,
    pub session_id: SessionId,
    /// Sequential display ID shown as `#N` in messages.
    pub short_id: u32,
    /// Agent backend name shown in messages (e.g. "kimi", "claude").
    pub agent_name: String,
    /// Wall-clock time the session was created (for display in `/sessions`).
    pub created_at: SystemTime,
    /// Timestamp of the last prompt dispatched; used for idle-timeout eviction.
    pub last_active: Instant,
    pub perm: PermissionPolicy,
}

impl ActiveSession {
    pub fn tag(&self) -> String {
        format!("[#{} {}]", self.short_id, self.agent_name)
    }
}

#[derive(Debug)]
pub struct PendingElicit {
    pub session_id: SessionId,
    pub elicit_id: String,
    pub peer: PeerRef,
    pub schema: Option<Vec<ElicitField>>,
}

/// A numbered list the user must choose from, produced by `/stop` or `/cd`
/// when called without arguments.
#[derive(Debug)]
pub struct PendingSelection {
    pub peer: PeerRef,
    pub action: SelectionAction,
    /// The actual values the user is choosing between.
    pub choices: Vec<String>,
}

#[derive(Debug)]
pub enum SelectionAction {
    /// Stop a session — the choice values are session IDs.
    Stop,
    /// Change directory — the choice values are absolute paths.
    Cd,
}
