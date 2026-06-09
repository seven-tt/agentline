//! DingTalk Stream API adapter for agentline.
//!
//! Speaks the Dingtalk gateway protocol directly — no third-party SDK.
//! Reference: https://github.com/open-dingtalk/dingtalk-stream-sdk-go
//! Inbound: WebSocket stream, CALLBACK topic `/v1.0/im/bot/messages/get`.
//! Outbound: per-message `sessionWebhook` HTTP POST.

pub mod auth;
pub mod error;
pub mod send;
pub mod stream;
pub mod types;

pub use auth::OpenParams;
pub use error::{Error, Result};
pub use stream::{StreamConfig, WebhookCache, spawn_stream};

use agentline_bridge::ImChannel;
use agentline_bridge::types::{Media, PeerRef};
use async_trait::async_trait;

/// Outbound side of the DingTalk adapter. Construct via `DingtalkChannel::start`,
/// which returns the channel together with the inbound mpsc receiver.
pub struct DingtalkChannel {
    http: reqwest::Client,
    webhooks: WebhookCache,
}

impl DingtalkChannel {
    /// Build the channel and spawn the stream loop. Returns
    /// `(channel, inbound_rx, join_handle)`.
    pub fn start(
        cfg: StreamConfig,
    ) -> Result<(
        Self,
        tokio::sync::mpsc::Receiver<agentline_bridge::types::InboundMessage>,
        tokio::task::JoinHandle<()>,
    )> {
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| Error::other(format!("build http: {e}")))?;
        let (rx, webhooks, handle) = spawn_stream(cfg);
        Ok((Self { http, webhooks }, rx, handle))
    }
}

#[async_trait]
impl ImChannel for DingtalkChannel {
    async fn send_text(&self, to: &PeerRef, text: &str) -> agentline_bridge::Result<()> {
        send::send_text(&self.http, &self.webhooks, to, text)
            .await
            .map_err(Into::into)
    }

    async fn send_media(&self, _to: &PeerRef, _media: Media) -> agentline_bridge::Result<()> {
        Err(agentline_bridge::Error::NotSupported)
    }
}
