use crate::permission::PermissionDanger;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::SystemTime;

/// Bridge-level operations. InputSources produce these from platform-specific
/// interactions (text commands, button clicks, CLI flags, JSON messages).
/// Bridge only receives and executes them — it never parses raw text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    /// `/cd <path>` — with explicit path
    Cd(PathBuf),
    /// `/cd` — no argument, trigger interactive directory listing
    CdInteractive,
    /// `/new`
    New,
    /// `/close [#N]` — close (destroy) a session. `/stop` is an alias.
    Close(Option<u32>),
    /// `/cancel` — cancel the running prompt but keep the session alive.
    Cancel,
    /// `/agent [name]` — switch agent backend, or list available agents.
    Agent(Option<String>),
    /// `/sessions` — list active sessions.
    Sessions,
    /// `/yolo`
    Yolo,
    /// `/safe`
    Safe,
    /// `/help` or `/?`
    Help,
    /// Bare-token affirmative: `y`, `yes`, `是`, `好`.
    YesToken,
    /// Bare-token negative: `n`, `no`, `否`, `不`.
    NoToken,
    /// Session-level approval: `s`, `session`, `会话`.
    SessionApprove,
    /// Anything else — including unknown slash-commands which are forwarded
    /// to the agent as-is.
    Plain(String),
}

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
    Thinking {
        text: String,
    },
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
    Number {
        minimum: Option<f64>,
        maximum: Option<f64>,
    },
}

/// One option in a single-select or multi-select field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElicitOption {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

// ── New types for the InputSource architecture ──────────────────────────

/// Parsed inbound payload: either a command or user content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InboundPayload {
    /// A command recognized by the InputSource (e.g. /yolo, button click, CLI flag).
    Command(Command),
    /// User content to be sent to the agent.
    Content(UserContent),
}

/// Composite user message: text + zero or more attachments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserContent {
    pub text: Option<String>,
    pub attachments: Vec<Attachment>,
}

impl UserContent {
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            text: Some(s.into()),
            attachments: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.as_ref().is_none_or(|t| t.is_empty()) && self.attachments.is_empty()
    }

    pub fn to_prompt_text(&self) -> String {
        let mut parts = Vec::new();
        if let Some(ref text) = self.text {
            parts.push(text.clone());
        }
        for att in &self.attachments {
            match att.kind {
                AttachmentKind::Image => {
                    parts.push(format!("![image]({})", att.local_path.display()));
                }
                AttachmentKind::Voice => {
                    if let Some(ref transcript) = att.transcript {
                        parts.push(transcript.clone());
                    } else {
                        parts.push(format!("[voice]({})", att.local_path.display()));
                    }
                }
                AttachmentKind::File => {
                    let name = att.name.as_deref().unwrap_or("file");
                    parts.push(format!("[file: {}]({})", name, att.local_path.display()));
                }
                AttachmentKind::Video => {
                    parts.push(format!("[video]({})", att.local_path.display()));
                }
            }
        }
        parts.join("\n")
    }
}

/// A single attachment in an inbound message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub kind: AttachmentKind,
    /// Local path (InputSource downloads media before handing off to Bridge).
    pub local_path: PathBuf,
    /// Display name (for files).
    pub name: Option<String>,
    /// Voice-to-text transcript (for voice attachments).
    pub transcript: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttachmentKind {
    Image,
    Voice,
    File,
    Video,
}

/// Inbound message with routing information (which InputSource it came from).
/// The payload is already parsed by the InputSource's `parse_message`.
#[derive(Debug, Clone)]
pub struct RoutedMessage {
    pub source_id: String,
    pub peer: PeerRef,
    pub payload: InboundPayload,
    pub received_at: std::time::SystemTime,
}

// ── Bridge atomic API result types ───────────────────────────────────

/// Result of `Bridge::set_yolo()`.
#[derive(Debug, Clone)]
pub enum YoloResult {
    /// Yolo applied to the active session.
    Applied { tag: String },
    /// No active session; yolo will apply to the next session created.
    Pending,
}

/// Result of `Bridge::set_safe()`.
#[derive(Debug, Clone)]
pub enum SafeResult {
    /// Safe mode applied to the active session (yolo off + grants cleared).
    Applied { tag: String },
    /// No active session.
    NoSession,
}

/// Result of `Bridge::set_cwd()`.
#[derive(Debug, Clone)]
pub enum CwdResult {
    /// cwd changed, no session impact.
    Changed,
    /// cwd changed and the active session was closed because cwd differs.
    SessionClosed { tag: String },
    /// Path does not exist.
    NotFound,
    /// Path is not a directory.
    NotADir,
}

/// Result of `Bridge::send_prompt()`.
#[derive(Debug, Clone)]
pub enum PromptResult {
    /// Prompt dispatched immediately.
    Started,
    /// Another prompt is running; this one was queued.
    Queued { tag: String },
}

/// Result of `Bridge::answer_permission()`.
#[derive(Debug, Clone)]
pub enum PermAnswerResult {
    /// Permission was answered.
    Answered {
        effective: crate::permission::PermResponse,
    },
    /// No pending permission request.
    NoPending,
}

/// Snapshot of the current session for external display.
#[derive(Debug, Clone)]
pub struct SessionInfoSnapshot {
    pub short_id: u32,
    pub session_id: String,
    pub agent_name: String,
    pub cwd: std::path::PathBuf,
    pub created_at: std::time::SystemTime,
    pub idle_duration: std::time::Duration,
    pub is_yolo: bool,
    pub grant_summary: String,
}

/// Parse user reply into a structured `serde_json::Value` based on the
/// elicitation schema. If schema carries selectable options, numeric input
/// is resolved to the corresponding option value.
pub fn parse_elicit_response(text: &str, schema: Option<&[ElicitField]>) -> serde_json::Value {
    let text = text.trim();
    let Some(fields) = schema else {
        return serde_json::Value::String(text.to_string());
    };
    let Some(field) = fields.first() else {
        return serde_json::Value::String(text.to_string());
    };
    match &field.field_type {
        ElicitFieldType::SingleSelect { options } => {
            if let Ok(n) = text.parse::<usize>()
                && n >= 1
                && n <= options.len()
            {
                return serde_json::Value::String(options[n - 1].value.clone());
            }
            serde_json::Value::String(text.to_string())
        }
        ElicitFieldType::MultiSelect { options } => {
            let indices: Vec<usize> = text
                .split(',')
                .filter_map(|s| s.trim().parse::<usize>().ok())
                .collect();
            if !indices.is_empty() && indices.iter().all(|&i| i >= 1 && i <= options.len()) {
                let values: Vec<serde_json::Value> = indices
                    .iter()
                    .map(|&i| serde_json::Value::String(options[i - 1].value.clone()))
                    .collect();
                return serde_json::Value::Array(values);
            }
            serde_json::Value::String(text.to_string())
        }
        ElicitFieldType::Boolean => {
            let lower = text.to_lowercase();
            match lower.as_str() {
                "y" | "yes" | "true" | "是" | "1" => serde_json::Value::Bool(true),
                "n" | "no" | "false" | "否" | "0" => serde_json::Value::Bool(false),
                _ => serde_json::Value::String(text.to_string()),
            }
        }
        ElicitFieldType::Number { .. } => {
            if let Ok(n) = text.parse::<f64>() {
                serde_json::Number::from_f64(n)
                    .map(serde_json::Value::Number)
                    .unwrap_or_else(|| serde_json::Value::String(text.to_string()))
            } else {
                serde_json::Value::String(text.to_string())
            }
        }
        ElicitFieldType::Text { .. } => serde_json::Value::String(text.to_string()),
    }
}
