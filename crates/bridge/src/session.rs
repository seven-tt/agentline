use crate::permission::PermissionPolicy;
use crate::types::{AgentSessionId, PeerRef, SessionId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime};

/// A channel through which a session is reachable: which input source it
/// belongs to and which IM-side peer to deliver outbound updates to.
///
/// A session may be bound to more than one channel — the same conversation
/// can be visible from multiple message channels. Outbound updates are
/// mirrored to every binding.
#[derive(Debug, Clone)]
pub struct ChannelBinding {
    pub source_id: String,
    pub peer: PeerRef,
}

/// Generate a fresh external session id (ACP routing id). Process-unique:
/// monotonic counter mixed with a wall-clock nanosecond stamp.
pub fn gen_session_id() -> SessionId {
    static SESSION_SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SESSION_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    SessionId::new(format!("sess-{nanos:x}-{n:x}"))
}

/// Per-session state, keyed by the external [`SessionId`].
#[derive(Debug)]
pub struct Session {
    /// External ACP routing id (the map key). Stable across agent-subprocess
    /// recycling, so clients can hold onto it.
    pub id: SessionId,
    /// Channels this session is reachable from. Outbound updates are mirrored
    /// to all of them. Always has at least one (the creation channel).
    pub bindings: Vec<ChannelBinding>,
    pub cwd: PathBuf,
    /// Agent-subprocess session id (from `AgentBackend::new_session`). May be
    /// re-created internally on idle-expiry without changing [`Session::id`].
    pub agent_session_id: AgentSessionId,
    pub short_id: u32,
    pub agent_name: String,
    pub created_at: SystemTime,
    pub last_active: Instant,
    pub perm: PermissionPolicy,
    /// True once the project list has been injected into the first prompt
    /// of this session. Automatically false for new sessions.
    pub project_context_sent: bool,
}

impl Session {
    pub fn tag(&self) -> String {
        format!("[#{} {}]", self.short_id, self.agent_name)
    }

    /// The primary (creation) channel binding.
    pub fn primary(&self) -> &ChannelBinding {
        &self.bindings[0]
    }
}

/// Manages multiple concurrent sessions keyed by external [`SessionId`].
#[derive(Debug)]
pub struct SessionManager {
    sessions: HashMap<SessionId, Session>,
    counter: u32,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            counter: 0,
        }
    }

    pub fn next_short_id(&mut self) -> u32 {
        self.counter += 1;
        self.counter
    }

    pub fn get(&self, id: &SessionId) -> Option<&Session> {
        self.sessions.get(id)
    }

    pub fn get_mut(&mut self, id: &SessionId) -> Option<&mut Session> {
        self.sessions.get_mut(id)
    }

    pub fn insert(&mut self, id: SessionId, session: Session) {
        self.sessions.insert(id, session);
    }

    pub fn remove(&mut self, id: &SessionId) -> Option<Session> {
        self.sessions.remove(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&SessionId, &Session)> {
        self.sessions.iter()
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    pub fn drain(&mut self) -> impl Iterator<Item = (SessionId, Session)> + '_ {
        self.sessions.drain()
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

pub fn format_timestamp() -> String {
    use time::OffsetDateTime;
    // Falls back to UTC only if the OS local offset can't be determined
    // (e.g. certain sandboxed/multi-threaded edge cases the `time` crate
    // refuses to trust) — session dirs should reflect the user's wall clock.
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

pub fn cleanup_old_sessions(base: &std::path::Path, keep: usize) {
    let entries = match std::fs::read_dir(base) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut dirs: Vec<String> = entries
        .filter_map(|e| {
            let e = e.ok()?;
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with("sess-") && e.path().is_dir() {
                Some(name)
            } else {
                None
            }
        })
        .collect();
    dirs.sort();
    if dirs.len() <= keep {
        return;
    }
    for name in &dirs[..dirs.len() - keep] {
        let p = base.join(name);
        tracing::debug!(dir=%p.display(), "removing old session directory");
        if let Err(e) = std::fs::remove_dir_all(&p) {
            tracing::warn!(error=%e, dir=%p.display(), "failed to remove old session dir");
        }
    }
}
