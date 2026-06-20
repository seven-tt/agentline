use crate::registry::{ImSnapshot, SessionRegistry, SessionSnapshot};
use crate::session::SessionManager;
use std::collections::HashMap;

pub(crate) fn publish_registry_from_sessions(
    registry: &SessionRegistry,
    sessions: &SessionManager,
) {
    let current = registry.snapshot();
    let mut by_source: HashMap<String, Vec<SessionSnapshot>> = HashMap::new();
    for (id, s) in sessions.iter() {
        // A session may be visible from multiple channels; record it under each.
        for b in &s.bindings {
            by_source
                .entry(b.source_id.clone())
                .or_default()
                .push(SessionSnapshot {
                    id: id.to_string(),
                    user: b.peer.user_id.clone(),
                    active: true,
                    cwd: s.cwd.display().to_string(),
                });
        }
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
