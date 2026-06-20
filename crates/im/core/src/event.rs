//! Presentation vocabulary for the IM rendering layer.
//!
//! These are *render actions* — the result of synthesizing the bridge's raw
//! [`agentline_bridge::AgentUpdate`] stream into displayable units (stream
//! lifecycle, thinking-time summaries, tool formatting, turn-end markers).
//! The bridge no longer knows about these; synthesis happens in
//! [`crate::render`].

use crate::types::Media;
use agent_client_protocol::ElicitationSchema;
use agentline_bridge::permission::PermissionDanger;
use agentline_bridge::types::ToolKind;

/// A single render action for an IM adapter to display.
#[derive(Debug, Clone)]
pub enum OutboundEvent {
    /// Agent thinking chunk — streamed as-is.
    Thinking { tag: String, text: String },
    /// Thinking phase ended; carries elapsed seconds for a one-line summary.
    ThinkingEnd { tag: String, elapsed_secs: f64 },
    /// A new streaming response is about to begin.
    StreamStart { tag: String },
    /// Streaming text fragment from the agent.
    StreamChunk { text: String },
    /// End of a streaming text sequence.
    StreamEnd,
    /// Complete text message (e.g. command replies / fallbacks).
    Text { content: String, format: TextFormat },
    /// Media message (image, file, video, etc.).
    Media(Media),
    /// Tool call lifecycle event.
    Tool(ToolEvent),
    /// Agent needs user permission for an action.
    PermissionRequest {
        id: String,
        what: String,
        danger: PermissionDanger,
        tool_kind: ToolKind,
    },
    /// Agent is asking the user a structured question.
    ElicitInput {
        id: String,
        prompt: String,
        schema: Option<ElicitationSchema>,
    },
    /// Agent switched mode (e.g. plan mode, code mode).
    ModeChanged { tag: String, mode_id: String },
    /// Agent session title update.
    SessionTitle { tag: String, title: String },
    /// Agent produced a plan.
    Plan { steps: Vec<String> },
    /// Current turn completed. `silent` is true when the agent produced no
    /// visible text response — IMs should show an explicit end marker.
    Done { silent: bool },
    /// Error during agent execution.
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextFormat {
    Plain,
    Markdown,
}

#[derive(Debug, Clone)]
pub enum ToolEvent {
    Start {
        id: String,
        kind: ToolKind,
        label: String,
    },
    Progress {
        id: String,
        output: String,
    },
    End {
        id: String,
        ok: bool,
        summary: Option<String>,
        kind: ToolKind,
        label: String,
    },
}
