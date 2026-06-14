use crate::error::Result;
use crate::event::OutboundEvent;
use crate::types::{InboundMessage, InboundPayload, PeerRef};
use async_trait::async_trait;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputSourceKind {
    Im,
    Remote,
    Local,
}

/// Unified abstraction for all input sources — IM platforms, remote clients,
/// local clients all implement this trait.
#[async_trait]
pub trait InputSource: Send + Sync + 'static {
    /// Unique identifier for this input source (e.g. "wechat", "feishu", "remote-ws").
    fn id(&self) -> &str;

    /// The kind of input source.
    fn kind(&self) -> InputSourceKind;

    /// Start the input source. Returns a receiver for inbound messages.
    /// The source runs in the background, pushing messages into the channel.
    async fn start(&self) -> Result<mpsc::Receiver<InboundMessage>>;

    /// Send a structured outbound event to a peer. Each InputSource renders
    /// the event according to its platform capabilities.
    async fn send_event(&self, to: &PeerRef, event: &OutboundEvent) -> Result<()>;

    /// Gracefully shut down the input source.
    async fn shutdown(&self) -> Result<()>;

    /// Parse an inbound message into a typed payload. IM sources use
    /// `agentline_im_core::default_parse_message` for the default text/media
    /// parsing; non-IM sources produce payloads directly.
    fn parse_message(&self, msg: &InboundMessage) -> InboundPayload;
}

/// IM-specific capabilities on top of InputSource.
#[async_trait]
pub trait ImAdapter: InputSource {
    /// Send plain text to a peer.
    async fn send_text(&self, to: &PeerRef, text: &str) -> Result<()>;

    /// Send Markdown-formatted text. Defaults to `send_text`.
    async fn send_markdown(&self, to: &PeerRef, text: &str) -> Result<()> {
        self.send_text(to, text).await
    }

    /// Send a typing indicator. Default is no-op.
    async fn typing(&self, _to: &PeerRef) -> Result<()> {
        Ok(())
    }

    /// How often to send typing indicators while the agent is working.
    /// Varies by platform (e.g. Telegram 5s, Feishu 60s).
    fn typing_interval(&self) -> Duration {
        Duration::from_secs(60)
    }

    /// Declare platform-specific rendering capabilities.
    fn capabilities(&self) -> ImCapabilities {
        ImCapabilities::default()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ImCapabilities {
    pub markdown: bool,
    pub streaming: bool,
    pub cards: bool,
}

/// Protocol-layer handler for inbound messages. Different protocols (IM, GUI,
/// CLI) implement this to compose Bridge atomic operations according to their
/// interaction model.
#[async_trait]
pub trait InboundHandler: Send + Sync {
    async fn handle(
        &self,
        bridge: &crate::bridge::Bridge,
        routed: crate::types::RoutedMessage,
    ) -> crate::error::Result<()>;
}
