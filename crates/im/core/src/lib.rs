//! Shared IM abstraction layer for agentline.
//!
//! Provides the default outbound renderer, inbound parser, command parsing,
//! and re-exports all bridge types that IM adapters need — so individual IM
//! crates depend only on `agentline-im-core`, not on `agentline-bridge` directly.

rust_i18n::i18n!("locales", fallback = "zh-CN");

pub mod commands;
pub mod handler;
mod parse;
mod render;

pub use parse::default_parse_message;
pub use render::render_outbound_event;

// ── Re-exports from agentline-bridge ────────────────────────────────────
//
// Only re-export types that IM adapters actually need.  Internal modules
// (handler, render, parse) import from `agentline_bridge` directly.

pub use agentline_bridge::{Error, Result};

pub mod event {
    pub use agentline_bridge::event::{OutboundEvent, TextFormat, ToolEvent};
}

pub mod source {
    pub use agentline_bridge::source::{ImAdapter, ImCapabilities, InputSource, InputSourceKind};
}

pub mod types {
    pub use agentline_bridge::types::{
        ElicitFieldType, InboundMessage, InboundPayload, MessageKind, PeerRef,
    };
}

pub mod format {
    pub use agentline_bridge::format::{fmt_ago, fmt_local, tool_label, truncate};
}

pub use agentline_bridge::permission::PermissionDanger;
pub use handler::ImInboundHandler;
