//! iroh P2P transport.
//!
//! Accepts connections over QUIC using iroh's P2P networking. Two ALPNs:
//! - `agentline/acp/1`   — ACP protocol
//! - `agentline/files/1` — file browsing protocol

use std::path::Path;
use std::sync::Arc;

use agentline_transport::{Protocol, TransportConn, TransportListener};
use async_trait::async_trait;
use iroh::{Endpoint, RelayMode, SecretKey};
use tokio::sync::Notify;

const ALPN_ACP: &[u8] = b"agentline/acp/1";
const ALPN_FILES: &[u8] = b"agentline/files/1";

pub struct IrohListener {
    endpoint: Endpoint,
    shutdown: Arc<Notify>,
}

impl IrohListener {
    pub async fn new(
        secret_key_hex: &str,
        key_path: &Path,
        relay_url: &str,
    ) -> agentline_bridge::Result<Self> {
        let secret_key = resolve_secret_key(secret_key_hex, key_path)?;

        let mut builder = Endpoint::builder(iroh::endpoint::presets::N0)
            .alpns(vec![ALPN_ACP.to_vec(), ALPN_FILES.to_vec()])
            .secret_key(secret_key);

        if !relay_url.is_empty() {
            let url: iroh::RelayUrl = relay_url
                .parse()
                .map_err(|e| agentline_bridge::Error::other(format!("bad relay_url: {e}")))?;
            builder = builder.relay_mode(RelayMode::custom([url]));
        }

        let endpoint = builder
            .bind()
            .await
            .map_err(|e| agentline_bridge::Error::other(format!("iroh bind: {e}")))?;

        let addr = endpoint.addr();
        tracing::info!(
            node_id = %addr.id,
            addrs = ?addr.addrs,
            "iroh transport listening",
        );

        Ok(Self {
            endpoint,
            shutdown: Arc::new(Notify::new()),
        })
    }

    pub fn node_id(&self) -> iroh::EndpointId {
        self.endpoint.id()
    }
}

fn resolve_secret_key(hex: &str, key_path: &Path) -> agentline_bridge::Result<SecretKey> {
    if !hex.is_empty() {
        return parse_secret_key_hex(hex);
    }
    if key_path.exists() {
        let contents = std::fs::read_to_string(key_path).map_err(|e| {
            agentline_bridge::Error::other(format!("read {}: {e}", key_path.display()))
        })?;
        return parse_secret_key_hex(contents.trim());
    }
    let key = SecretKey::generate();
    if let Some(parent) = key_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let hex_str = hex::encode(key.to_bytes());
    std::fs::write(key_path, &hex_str).map_err(|e| {
        agentline_bridge::Error::other(format!("write {}: {e}", key_path.display()))
    })?;
    tracing::info!(path=%key_path.display(), "generated and saved iroh secret key");
    Ok(key)
}

fn parse_secret_key_hex(hex_str: &str) -> agentline_bridge::Result<SecretKey> {
    let bytes: [u8; 32] = hex::decode(hex_str)
        .map_err(|e| agentline_bridge::Error::other(format!("bad secret_key hex: {e}")))?
        .try_into()
        .map_err(|v: Vec<u8>| {
            agentline_bridge::Error::other(format!("secret_key must be 32 bytes, got {}", v.len()))
        })?;
    Ok(SecretKey::from_bytes(&bytes))
}

#[async_trait]
impl TransportListener for IrohListener {
    async fn accept(&self) -> agentline_bridge::Result<TransportConn> {
        tokio::select! {
            incoming = self.endpoint.accept() => {
                let incoming = incoming.ok_or_else(|| {
                    agentline_bridge::Error::other("iroh endpoint closed")
                })?;
                let mut accepting = incoming.accept().map_err(|e| {
                    agentline_bridge::Error::other(format!("iroh accept: {e}"))
                })?;
                let alpn = accepting.alpn().await.map_err(|e| {
                    agentline_bridge::Error::other(format!("iroh alpn: {e}"))
                })?;
                let protocol = if alpn == ALPN_FILES {
                    Protocol::Files
                } else {
                    Protocol::Acp
                };
                let conn = accepting.await.map_err(|e| {
                    agentline_bridge::Error::other(format!("iroh connect: {e}"))
                })?;
                let (send, recv) = conn.accept_bi().await.map_err(|e| {
                    agentline_bridge::Error::other(format!("iroh accept_bi: {e}"))
                })?;
                Ok(TransportConn {
                    protocol,
                    read: Box::new(recv),
                    write: Box::new(send),
                })
            }
            () = self.shutdown.notified() => {
                Err(agentline_bridge::Error::other("iroh listener shutting down"))
            }
        }
    }

    async fn shutdown(&self) -> agentline_bridge::Result<()> {
        self.shutdown.notify_one();
        self.endpoint.close().await;
        tracing::info!("iroh transport closed");
        Ok(())
    }
}
