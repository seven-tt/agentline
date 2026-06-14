use crate::permission::PermissionPolicy;
use crate::types::{PeerRef, SessionId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Instant, SystemTime};

/// Session key: identifies a unique conversation context.
/// Private chat = (source_id, user_id, None).
/// Group chat = (source_id, user_id, Some(group_id)).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionKey {
    pub source_id: String,
    pub user_id: String,
    pub group_id: Option<String>,
}

impl SessionKey {
    pub fn new(source_id: impl Into<String>, peer: &PeerRef) -> Self {
        Self {
            source_id: source_id.into(),
            user_id: peer.user_id.clone(),
            group_id: peer.group_id.clone(),
        }
    }
}

/// Per-session state, keyed by SessionKey.
#[derive(Debug)]
pub struct ManagedSession {
    pub peer: PeerRef,
    pub cwd: PathBuf,
    pub session_id: SessionId,
    pub short_id: u32,
    pub agent_name: String,
    pub created_at: SystemTime,
    pub last_active: Instant,
    pub perm: PermissionPolicy,
    /// True once the project list has been injected into the first prompt
    /// of this session. Automatically false for new sessions.
    pub project_context_sent: bool,
}

impl ManagedSession {
    pub fn tag(&self) -> String {
        format!("[#{} {}]", self.short_id, self.agent_name)
    }
}

/// Manages multiple concurrent sessions keyed by (source_id, user_id, group_id).
#[derive(Debug)]
pub struct SessionManager {
    sessions: HashMap<SessionKey, ManagedSession>,
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

    pub fn get(&self, key: &SessionKey) -> Option<&ManagedSession> {
        self.sessions.get(key)
    }

    pub fn get_mut(&mut self, key: &SessionKey) -> Option<&mut ManagedSession> {
        self.sessions.get_mut(key)
    }

    pub fn insert(&mut self, key: SessionKey, session: ManagedSession) {
        self.sessions.insert(key, session);
    }

    pub fn remove(&mut self, key: &SessionKey) -> Option<ManagedSession> {
        self.sessions.remove(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&SessionKey, &ManagedSession)> {
        self.sessions.iter()
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    pub fn drain(&mut self) -> impl Iterator<Item = (SessionKey, ManagedSession)> + '_ {
        self.sessions.drain()
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

pub fn format_timestamp() -> String {
    use std::time::SystemTime;
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let z = secs as i64 / 86400 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let time = secs % 86400;
    let h = time / 3600;
    let min = (time % 3600) / 60;
    let s = time % 60;
    format!("{y:04}{m:02}{d:02}-{h:02}{min:02}{s:02}")
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
