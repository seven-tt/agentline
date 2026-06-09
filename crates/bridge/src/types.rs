use crate::permission::PermissionDanger;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::SystemTime;

/// A project the agent can be directed to clone and develop.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Project {
    pub name: String,
    pub git_url: String,
}

/// Stable identifier for an agent session, returned by AgentBackend::new_session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub String);

impl SessionId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifies the IM-side counterpart of a conversation.
///
/// `opaque` carries platform-specific context (e.g. iLink's `context_token`)
/// that the bridge doesn't interpret but the IM adapter needs back when sending.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerRef {
    pub user_id: String,
    pub group_id: Option<String>,
    pub opaque: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundMessage {
    pub peer: PeerRef,
    pub kind: MessageKind,
    #[serde(default = "SystemTime::now")]
    pub received_at: SystemTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageKind {
    Text {
        text: String,
    },
    Image {
        /// Local path to the downloaded image (IM layer is responsible for download).
        local_path: Option<PathBuf>,
        caption: Option<String>,
    },
    Voice {
        transcript: Option<String>,
        /// Local path to the downloaded voice file (IM layer is responsible for download).
        local_path: Option<PathBuf>,
    },
    File {
        local_path: PathBuf,
        name: String,
    },
    Video {
        local_path: Option<PathBuf>,
        caption: Option<String>,
    },
}

impl MessageKind {
    /// Best-effort text representation. None for raw media without caption/transcript.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            MessageKind::Text { text } => Some(text),
            MessageKind::Voice {
                transcript: Some(t),
                ..
            } => Some(t),
            MessageKind::Image {
                caption: Some(c), ..
            } => Some(c),
            MessageKind::Video {
                caption: Some(c), ..
            } => Some(c),
            _ => None,
        }
    }
}

/// Outbound media payload for ImChannel::send_media.
#[derive(Debug, Clone)]
pub struct Media {
    pub kind: MediaKind,
    pub local_path: PathBuf,
    pub caption: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    File,
    Video,
    Voice,
}

/// One frame of progress from the agent during a prompt.
#[derive(Debug, Clone)]
pub enum AgentUpdate {
    /// Streaming assistant prose. Many small deltas, then a final one with `is_final: true`.
    AssistantText {
        delta: String,
        is_final: bool,
    },
    /// Agent reasoning / thinking chunk (not shown to user verbatim).
    Thinking { text: String },
    ToolCallStart {
        id: String,
        kind: ToolKind,
        label: String,
    },
    ToolCallProgress {
        id: String,
        output_chunk: String,
    },
    ToolCallEnd {
        id: String,
        ok: bool,
        summary: Option<String>,
    },
    Plan {
        steps: Vec<String>,
    },
    PermissionRequest {
        id: String,
        what: String,
        danger: PermissionDanger,
        tool_kind: ToolKind,
    },
    /// Agent switched execution mode (e.g. "plan", "compact").
    ModeChanged {
        mode_id: String,
    },
    /// Agent updated session metadata (title / info text).
    SessionInfo {
        title: String,
    },
    /// Agent is asking the user a question (ACP elicitation).
    ElicitInput {
        id: String,
        prompt: String,
        schema: Option<Vec<ElicitField>>,
    },
    Done,
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolKind {
    Shell,
    FileRead,
    FileEdit,
    FileWrite,
    Search,
    Web,
    Other,
}

impl ToolKind {
    pub fn emoji(self) -> &'static str {
        match self {
            ToolKind::Shell => "🔧",
            ToolKind::FileRead => "📖",
            ToolKind::FileEdit => "✏️",
            ToolKind::FileWrite => "📝",
            ToolKind::Search => "🔍",
            ToolKind::Web => "🌐",
            ToolKind::Other => "⚙️",
        }
    }

    /// Stable English variant name shown in tool-call labels, e.g. `Shell`,
    /// `FileRead`. Pairs with [`ToolKind::emoji`] in [`crate::format::tool_label`].
    pub fn name(self) -> &'static str {
        match self {
            ToolKind::Shell => "Shell",
            ToolKind::FileRead => "FileRead",
            ToolKind::FileEdit => "FileEdit",
            ToolKind::FileWrite => "FileWrite",
            ToolKind::Search => "Search",
            ToolKind::Web => "Web",
            ToolKind::Other => "Other",
        }
    }
}

/// Neutral message event sent from the bridge to an IM adapter.
///
/// Bridge translates agent-specific `AgentUpdate` frames into these IM-neutral
/// events. Each adapter decides how to map them to its platform wire format.
#[derive(Debug, Clone)]
pub enum MessageEvent {
    /// A streaming text delta produced by the agent in real time.
    StreamChunk { text: String },
    /// Signals the end of a streaming text sequence.
    StreamEnd,
    /// A complete non-streaming text (e.g. command replies like /help).
    PlainText(String),
    /// A tool invocation has started.
    ToolStart { id: String, kind: ToolKind, label: String },
    /// Raw stdout/stderr chunk from a running tool (often suppressed by IMs).
    ToolProgress { id: String, output: String },
    /// A tool invocation has finished.
    ToolEnd { id: String, ok: bool, summary: Option<String> },
    /// The agent produced a plan.
    Plan { steps: Vec<String> },
    /// The agent needs user permission for a dangerous action.
    PermissionRequest { id: String, what: String, danger: PermissionDanger, tag: String },
    /// The agent is asking the user a structured question (elicitation).
    ElicitInput { id: String, prompt: String, schema: Option<Vec<ElicitField>> },
    /// The agent turn has completed.
    Done,
    /// An error occurred inside the agent.
    Error(String),
}

/// A single field in an elicitation form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElicitField {
    pub key: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub required: bool,
    pub field_type: ElicitFieldType,
}

/// The type of input expected for an elicitation field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ElicitFieldType {
    /// Free-text input (optionally with format hint like "email", "uri", "date").
    Text { format: Option<String> },
    /// Single-select from a list of options.
    SingleSelect { options: Vec<ElicitOption> },
    /// Multi-select from a list of options.
    MultiSelect { options: Vec<ElicitOption> },
    /// Boolean yes/no.
    Boolean,
    /// Numeric input.
    Number { minimum: Option<f64>, maximum: Option<f64> },
}

/// One option in a single-select or multi-select field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElicitOption {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}
