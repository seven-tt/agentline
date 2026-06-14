use crate::permission::PermissionDanger;
use crate::types::{ElicitField, Media, SessionInfoSnapshot, ToolKind};

/// Bridge → InputSource outbound events. Semantic, not formatted — each
/// InputSource decides how to render these for its platform.
#[derive(Debug, Clone)]
pub enum OutboundEvent {
    /// Agent thinking chunk — streamed as-is. Each InputSource decides
    /// whether to display incrementally (e.g. Feishu streaming card) or
    /// buffer and summarize (e.g. WeChat).
    Thinking { tag: String, text: String },
    /// Thinking phase ended. Carries total elapsed seconds so platforms
    /// that buffer can emit a one-line summary.
    ThinkingEnd { tag: String, elapsed_secs: f64 },
    /// A new streaming response is about to begin (carries the session tag).
    StreamStart { tag: String },
    /// Streaming text fragment from the agent.
    StreamChunk { text: String },
    /// End of a streaming text sequence.
    StreamEnd,
    /// Complete text message (e.g. command replies).
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
        schema: Option<Vec<ElicitField>>,
    },
    /// Agent switched mode (e.g. plan mode, code mode).
    ModeChanged { tag: String, mode_id: String },
    /// Agent session title update.
    SessionTitle { tag: String, title: String },
    /// Agent produced a plan.
    Plan { steps: Vec<String> },
    /// Session list / status display — each IM formats to its own style.
    SessionList { info: Option<SessionInfoSnapshot> },
    /// Current turn completed. `silent` is true when the agent produced no
    /// text response (e.g. after a permission denial) — IMs should show an
    /// explicit end marker so the user knows the turn is over.
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
