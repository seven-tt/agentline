use crate::types::{AgentUpdate, SessionId};

/// A semantic agent event delivered from the bridge to an InputSource.
///
/// The bridge emits concrete operation types only — the raw [`AgentUpdate`]
/// plus the owning session id and its display tag. All presentation
/// (stream lifecycle, thinking-time summaries, tool formatting, turn-end
/// markers) is synthesized by the rendering layer inside the IM adapters,
/// not here. This keeps the bridge a pure protocol layer.
#[derive(Debug, Clone)]
pub struct AgentEvent {
    /// The session this event belongs to.
    pub session_id: SessionId,
    /// Human-facing session tag, e.g. `[#1 claude]`.
    pub tag: String,
    /// The raw agent operation.
    pub update: AgentUpdate,
}

impl AgentEvent {
    pub fn new(session_id: SessionId, tag: impl Into<String>, update: AgentUpdate) -> Self {
        Self {
            session_id,
            tag: tag.into(),
            update,
        }
    }
}
