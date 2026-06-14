//! Neutral core for agentline: traits and a runtime that bridges any IM
//! adapter to any coding-agent adapter.

pub mod agent;
pub mod bridge;
pub mod error;
pub mod event;
pub mod format;
pub mod permission;
pub mod process;
pub mod proxy;
pub mod registry;
mod relay;
pub mod router;
mod scheduler;
pub mod session;
pub mod source;
pub mod state;
pub mod types;

pub use agent::{AgentBackend, AgentFactory, AgentInfo, AgentStatus};
pub use bridge::{Bridge, BridgeConfig};
pub use error::{Error, Result};
pub use event::{OutboundEvent, TextFormat, ToolEvent};
pub use permission::{AutoApprove, PendingPerm, PermResponse, PermissionDanger, PermissionPolicy};
pub use registry::{ImSnapshot, SessionRegistry, SessionSnapshot};
pub use router::SourceRouter;
pub use session::{ManagedSession, SessionKey, SessionManager};
pub use source::{ImAdapter, ImCapabilities, InboundHandler, InputSource, InputSourceKind};
pub use types::Command;
pub use types::{
    AgentUpdate, Attachment, AttachmentKind, CwdResult, ElicitField, ElicitFieldType, ElicitOption,
    InboundMessage, InboundPayload, Media, MediaKind, MessageKind, PeerRef, PermAnswerResult,
    Project, PromptResult, RoutedMessage, SafeResult, SessionId, SessionInfoSnapshot, ToolKind,
    UserContent, YoloResult,
};
