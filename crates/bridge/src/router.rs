use crate::error::{Error, Result};
use crate::event::AgentEvent;
use crate::source::{ImAdapter, InputSource};
use crate::types::{PeerRef, RoutedMessage};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;

/// Bridge-internal protocol abstraction layer. Manages all input sources
/// (IM + remote/local clients), merges inbound messages into a unified stream,
/// and routes outbound events by source_id.
///
/// `sources` uses `RwLock` to support dynamic registration of ACP transport
/// connections after the bridge is running. `im_adapters` is set only at
/// startup and never changes.
pub struct SourceRouter {
    sources: RwLock<HashMap<String, Arc<dyn InputSource>>>,
    im_adapters: HashMap<String, Arc<dyn ImAdapter>>,
}

impl SourceRouter {
    pub fn new() -> Self {
        Self {
            sources: RwLock::new(HashMap::new()),
            im_adapters: HashMap::new(),
        }
    }

    /// Register an IM input source (writes to both sources and im_adapters).
    /// Only called during setup, before the router is shared.
    pub fn register_im(&mut self, adapter: Arc<dyn ImAdapter>) {
        let id = adapter.id().to_string();
        self.sources
            .write()
            .unwrap()
            .insert(id.clone(), adapter.clone() as Arc<dyn InputSource>);
        self.im_adapters.insert(id, adapter);
    }

    /// Register a non-IM input source. Safe to call after the router is
    /// wrapped in `Arc` (used by ACP transport connections).
    pub fn register_source(&self, source: Arc<dyn InputSource>) {
        self.sources
            .write()
            .unwrap()
            .insert(source.id().to_string(), source);
    }

    /// Unregister a source by id. Used when an ACP transport connection
    /// disconnects.
    pub fn unregister_source(&self, id: &str) {
        self.sources.write().unwrap().remove(id);
    }

    /// Start all currently registered input sources and merge their pre-parsed
    /// messages into a unified channel of `RoutedMessage`.
    pub async fn start(&self) -> Result<mpsc::Receiver<RoutedMessage>> {
        let (tx, rx) = mpsc::channel(128);
        let snapshot: Vec<_> = self.sources.read().unwrap().clone().into_iter().collect();
        for (id, source) in snapshot {
            let mut source_rx = source.start().await?;
            let tx = tx.clone();
            tokio::spawn(async move {
                while let Some(msg) = source_rx.recv().await {
                    let _ = tx
                        .send(RoutedMessage {
                            source_id: id.clone(),
                            peer: msg.peer,
                            payload: msg.payload,
                            received_at: msg.received_at,
                        })
                        .await;
                }
            });
        }
        Ok(rx)
    }

    /// Route a semantic agent event to the correct InputSource by source_id.
    pub async fn send_update(
        &self,
        source_id: &str,
        to: &PeerRef,
        event: &AgentEvent,
    ) -> Result<()> {
        let source = self
            .sources
            .read()
            .unwrap()
            .get(source_id)
            .cloned()
            .ok_or_else(|| Error::other(format!("unknown source: {source_id}")))?;
        source.send_update(to, event).await
    }

    /// Send a plain-text (or markdown) reply directly to an IM peer. Used by
    /// the inbound handler for command responses, which are not part of the
    /// agent stream.
    pub async fn reply_text(
        &self,
        source_id: &str,
        to: &PeerRef,
        text: &str,
        markdown: bool,
    ) -> Result<()> {
        let im = self
            .im_adapters
            .get(source_id)
            .ok_or_else(|| Error::other(format!("unknown im source: {source_id}")))?;
        if markdown {
            im.send_markdown(to, text).await
        } else {
            im.send_text(to, text).await
        }
    }

    /// Send the `/sessions` reply, letting the IM adapter render `info`
    /// natively (falls back to `fallback_markdown` if it doesn't override).
    pub async fn reply_session_info(
        &self,
        source_id: &str,
        to: &PeerRef,
        info: Option<&crate::types::SessionInfo>,
        fallback_markdown: &str,
    ) -> Result<()> {
        let im = self
            .im_adapters
            .get(source_id)
            .ok_or_else(|| Error::other(format!("unknown im source: {source_id}")))?;
        im.send_session_info(to, info, fallback_markdown).await
    }

    /// Send the `/agent` list reply, letting the IM adapter render it
    /// natively (falls back to `fallback_markdown` if it doesn't override).
    pub async fn reply_agent_list(
        &self,
        source_id: &str,
        to: &PeerRef,
        current: &str,
        agents: &[crate::agent::AgentInfo],
        fallback_markdown: &str,
    ) -> Result<()> {
        let im = self
            .im_adapters
            .get(source_id)
            .ok_or_else(|| Error::other(format!("unknown im source: {source_id}")))?;
        im.send_agent_list(to, current, agents, fallback_markdown)
            .await
    }

    /// Get the IM adapter for a source (returns None for non-IM sources).
    pub fn get_im_adapter(&self, source_id: &str) -> Option<&Arc<dyn ImAdapter>> {
        self.im_adapters.get(source_id)
    }

    /// Shut down all input sources.
    pub async fn shutdown(&self) {
        let sources: Vec<_> = self.sources.read().unwrap().values().cloned().collect();
        for source in &sources {
            if let Err(e) = source.shutdown().await {
                tracing::warn!(source=%source.id(), error=%e, "source shutdown error");
            }
        }
    }

    /// List all registered source IDs.
    pub fn source_ids(&self) -> Vec<String> {
        self.sources.read().unwrap().keys().cloned().collect()
    }
}

impl Default for SourceRouter {
    fn default() -> Self {
        Self::new()
    }
}
