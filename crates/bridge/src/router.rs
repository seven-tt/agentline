use crate::error::{Error, Result};
use crate::event::OutboundEvent;
use crate::source::{ImAdapter, InputSource};
use crate::types::{PeerRef, RoutedMessage};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Bridge-internal protocol abstraction layer. Manages all input sources
/// (IM + remote/local clients), merges inbound messages into a unified stream,
/// and routes outbound events by source_id.
pub struct SourceRouter {
    sources: HashMap<String, Arc<dyn InputSource>>,
    im_adapters: HashMap<String, Arc<dyn ImAdapter>>,
}

impl SourceRouter {
    pub fn new() -> Self {
        Self {
            sources: HashMap::new(),
            im_adapters: HashMap::new(),
        }
    }

    /// Register an IM input source (writes to both sources and im_adapters).
    pub fn register_im(&mut self, adapter: Arc<dyn ImAdapter>) {
        let id = adapter.id().to_string();
        self.sources
            .insert(id.clone(), adapter.clone() as Arc<dyn InputSource>);
        self.im_adapters.insert(id, adapter);
    }

    /// Register a non-IM input source (remote client, local client, etc.).
    pub fn register_source(&mut self, source: Arc<dyn InputSource>) {
        self.sources.insert(source.id().to_string(), source);
    }

    /// Start all input sources and merge their inbound messages into a
    /// unified channel of `RoutedMessage`. Each message is parsed by the
    /// source's `parse_message` before forwarding.
    pub async fn start(&self) -> Result<mpsc::Receiver<RoutedMessage>> {
        let (tx, rx) = mpsc::channel(128);
        for (id, source) in &self.sources {
            let mut source_rx = source.start().await?;
            let tx = tx.clone();
            let id = id.clone();
            let source = source.clone();
            tokio::spawn(async move {
                while let Some(msg) = source_rx.recv().await {
                    let payload = source.parse_message(&msg);
                    let _ = tx
                        .send(RoutedMessage {
                            source_id: id.clone(),
                            peer: msg.peer,
                            payload,
                            received_at: msg.received_at,
                        })
                        .await;
                }
            });
        }
        Ok(rx)
    }

    /// Route an outbound event to the correct InputSource by source_id.
    pub async fn send_event(
        &self,
        source_id: &str,
        to: &PeerRef,
        event: &OutboundEvent,
    ) -> Result<()> {
        let source = self
            .sources
            .get(source_id)
            .ok_or_else(|| Error::other(format!("unknown source: {source_id}")))?;
        source.send_event(to, event).await
    }

    /// Get the IM adapter for a source (returns None for non-IM sources).
    pub fn get_im_adapter(&self, source_id: &str) -> Option<&Arc<dyn ImAdapter>> {
        self.im_adapters.get(source_id)
    }

    /// Shut down all input sources.
    pub async fn shutdown(&self) {
        for source in self.sources.values() {
            if let Err(e) = source.shutdown().await {
                tracing::warn!(source=%source.id(), error=%e, "source shutdown error");
            }
        }
    }

    /// List all registered source IDs.
    pub fn source_ids(&self) -> Vec<String> {
        self.sources.keys().cloned().collect()
    }
}

impl Default for SourceRouter {
    fn default() -> Self {
        Self::new()
    }
}
