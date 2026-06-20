use crate::error::Result;
use crate::event::AgentEvent;
use crate::types::{PeerRef, SourceMessage};
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

    /// Start the input source. Returns a receiver for pre-parsed inbound
    /// messages. Each source parses its platform-specific format internally
    /// before sending [`SourceMessage`]s.
    async fn start(&self) -> Result<mpsc::Receiver<SourceMessage>>;

    /// Deliver a semantic agent event to a peer. The InputSource's rendering
    /// layer synthesizes presentation and displays it per platform.
    async fn send_update(&self, to: &PeerRef, event: &AgentEvent) -> Result<()>;

    /// Gracefully shut down the input source.
    async fn shutdown(&self) -> Result<()>;
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

    /// Render the `/sessions` reply. `info` is the structured snapshot so a
    /// platform can build its own native presentation (e.g. a Feishu fields
    /// card); `fallback_markdown` is a pre-rendered Markdown table for
    /// platforms that don't need to customize this. This crate has no i18n
    /// support, so the fallback text is built by the caller (the IM layer),
    /// not here.
    async fn send_session_info(
        &self,
        to: &PeerRef,
        _info: Option<&crate::types::SessionInfo>,
        fallback_markdown: &str,
    ) -> Result<()> {
        self.send_markdown(to, fallback_markdown).await
    }

    /// Render the `/agent` list reply. Same fallback contract as
    /// [`ImAdapter::send_session_info`].
    async fn send_agent_list(
        &self,
        to: &PeerRef,
        _current: &str,
        _agents: &[crate::agent::AgentInfo],
        fallback_markdown: &str,
    ) -> Result<()> {
        self.send_markdown(to, fallback_markdown).await
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

    /// Called when the bridge invalidates every session it's tracking (e.g.
    /// `/agent` switching backend drains them all). Handlers that keep their
    /// own peer → SessionId cache (as `ImInboundHandler` does, for the
    /// single-session-per-peer IM model) must drop it here too — otherwise
    /// they keep handing the bridge ids it no longer recognizes. Default
    /// no-op for handlers that don't cache session ids themselves.
    async fn invalidate_all_sessions(&self) {}
}
