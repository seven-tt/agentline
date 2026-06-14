use crate::registry::{ImSnapshot, SessionRegistry, SessionSnapshot};
use crate::session::SessionManager;
use std::collections::HashMap;

pub(crate) fn publish_registry_from_sessions(
    registry: &SessionRegistry,
    sessions: &SessionManager,
) {
    let current = registry.snapshot();
    let mut by_source: HashMap<String, Vec<SessionSnapshot>> = HashMap::new();
    for (key, a) in sessions.iter() {
        by_source
            .entry(key.source_id.clone())
            .or_default()
            .push(SessionSnapshot {
                id: a.session_id.as_str().to_string(),
                user: a.peer.user_id.clone(),
                active: true,
                cwd: a.cwd.display().to_string(),
            });
    }
    let mut data: HashMap<String, ImSnapshot> = current
        .into_iter()
        .map(|(id, snap)| {
            (
                id,
                ImSnapshot {
                    healthy: snap.healthy,
                    sessions: vec![],
                },
            )
        })
        .collect();
    for (id, sess) in by_source {
        data.insert(
            id,
            ImSnapshot {
                healthy: true,
                sessions: sess,
            },
        );
    }
    registry.replace(data);
}
