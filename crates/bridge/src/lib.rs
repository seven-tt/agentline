//! Neutral core for agentline: traits and a runtime that bridges any IM
//! adapter to any coding-agent adapter.

rust_i18n::i18n!("locales", fallback = "zh-CN");

pub mod agent;
pub mod bridge;
pub mod commands;
pub mod error;
pub mod format;
pub mod im;
pub mod permission;
pub mod process;
pub mod proxy;
pub mod registry;
pub mod state;
pub mod types;

pub use agent::AgentBackend;
pub use bridge::{Bridge, BridgeConfig};
pub use commands::Command;
pub use error::{Error, Result};
pub use im::ImChannel;
pub use permission::{AutoApprove, PendingPerm, PermResponse, PermissionDanger, PermissionPolicy};
pub use registry::{ImSnapshot, SessionRegistry, SessionSnapshot};
pub use types::{
    AgentUpdate, ElicitField, ElicitFieldType, ElicitOption, InboundMessage, Media, MediaKind,
    MessageEvent, MessageKind, PeerRef, Project, SessionId, ToolKind,
};
