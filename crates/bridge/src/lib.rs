//! Neutral core for agentline: traits and a runtime that bridges any IM
//! adapter to any coding-agent adapter.

pub mod acp_client;
pub mod acp_mapping;
pub mod acp_server;
pub mod agent;
pub mod bridge;
pub mod driver;
pub mod error;
pub mod event;
pub mod format;
pub mod permission;
pub mod process;
pub mod proxy;
pub mod raw_log;
pub mod registry;
mod relay;
pub mod router;
mod scheduler;
pub mod session;
pub mod source;
pub mod state;
pub mod transport;
pub mod types;

pub use agent::{
    AgentBackend, AgentBuildContext, AgentFactory, AgentInfo, AgentPlugin, AgentPluginRegistry,
    AgentStatus, AuthStatus, CredentialInfo, CredentialUpdate, PluginAgentFactory, detect_command,
};
pub use bridge::{Bridge, BridgeConfig};
pub use driver::{AcpBackend, AcpCodec, ToolCallParser};
pub use error::{Error, Result};
pub use event::AgentEvent;
pub use permission::{
    AutoApprove, PendingPermissionRequest, PermissionDanger, PermissionPolicy, PermissionResponse,
};
pub use registry::{ImSnapshot, SessionRegistry, SessionSnapshot};
pub use router::SourceRouter;
pub use session::{ChannelBinding, Session, SessionManager};
pub use source::{ImAdapter, ImCapabilities, InboundHandler, InputSource, InputSourceKind};
pub use types::Command;
pub use types::{
    AcpToolKind, AgentSessionId, AgentUpdate, Attachment, AttachmentKind, ContentChunk, CwdResult,
    ElicitationPropertySchema, EnumOption, InboundPayload, ModeResult, PeerRef, PermissionResult,
    Plan, PlanEntry, PlanEntryPriority, PlanEntryStatus, Project, PromptResult, RoutedMessage,
    SessionId, SessionInfo, SessionUpdate, SourceMessage, ToolCall, ToolCallStatus, ToolCallUpdate,
    ToolCallUpdateFields, ToolKind, UserContent,
};
