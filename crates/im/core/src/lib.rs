//! Shared IM abstraction layer for agentline.
//!
//! Provides the default outbound renderer, inbound parser, command parsing,
//! and re-exports all bridge types that IM adapters need — so individual IM
//! crates depend only on `agentline-im-core`, not on `agentline-bridge` directly.

rust_i18n::i18n!("locales", fallback = "zh-CN");

pub mod commands;
pub mod event;
pub mod handler;
mod parse;
mod render;
pub mod renderer;

pub use parse::default_parse_message;
pub use parse::parse_inbound;
pub use render::render_outbound_event;
pub use renderer::{RenderState, synthesize};

// ── Re-exports from agentline-bridge ────────────────────────────────────
//
// Only re-export types that IM adapters actually need.  Internal modules
// (handler, render, parse) import from `agentline_bridge` directly.

pub use agentline_bridge::{AgentEvent, Error, Result};

pub mod source {
    pub use agentline_bridge::source::{ImAdapter, ImCapabilities, InputSource, InputSourceKind};
}

pub mod types {
    pub use agent_client_protocol::ElicitationPropertySchema;
    pub use agentline_bridge::types::{
        AgentUpdate, InboundPayload, PeerRef, SessionId, SessionInfo, SourceMessage,
        multi_select_options, single_select_options,
    };
    pub use agentline_bridge::{AgentInfo, AgentStatus};

    use std::path::PathBuf;
    use std::time::SystemTime;

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

    /// Raw inbound message from an IM platform. IM-specific: carries the
    /// platform's native message format (text, image, voice, file, video).
    /// Stream modules construct this, then convert to [`SourceMessage`] via
    /// [`super::parse_inbound`] before handing off to the bridge.
    #[derive(Debug, Clone)]
    pub struct InboundMessage {
        pub peer: PeerRef,
        pub kind: MessageKind,
        pub received_at: SystemTime,
    }

    #[derive(Debug, Clone)]
    pub enum MessageKind {
        Text {
            text: String,
        },
        Image {
            local_path: Option<PathBuf>,
            caption: Option<String>,
        },
        Voice {
            transcript: Option<String>,
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
}

pub mod format {
    pub use agentline_bridge::format::{fmt_ago, fmt_local, tool_label, truncate};
}

pub use agentline_bridge::permission::PermissionDanger;
pub use handler::ImInboundHandler;
