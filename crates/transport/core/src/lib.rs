//! Multi-protocol transport layer.
//!
//! Defines [`TransportListener`] — a trait for transports that accept
//! connections — and [`spawn_transport`] which runs the accept loop on a
//! dedicated thread.
//!
//! Each connection carries a [`Protocol`] tag so the dispatcher can route
//! ACP connections to `serve_acp` and Files connections to the file browser.
//!
//! When a `token` is provided, every connection goes through a
//! challenge-response handshake and all subsequent data is HMAC-signed.
//!
//! Built-in transports: [`UnixSocketListener`] (unix-only), iroh (separate crate).

pub mod auth;
pub mod files;
#[cfg(unix)]
mod unix;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use agentline_bridge::Bridge;
use agentline_bridge::acp_server::{AcpSource, serve_acp};
use async_trait::async_trait;

#[cfg(unix)]
pub use unix::UnixSocketListener;

// ── Protocol + connection ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Acp,
    Files,
}

pub struct TransportConn {
    pub protocol: Protocol,
    pub read: Box<dyn tokio::io::AsyncRead + Unpin + Send + 'static>,
    pub write: Box<dyn tokio::io::AsyncWrite + Unpin + Send + 'static>,
}

// ── TransportListener trait ────────────────────────────────────────────

#[async_trait]
pub trait TransportListener: Send + Sync + 'static {
    async fn accept(&self) -> agentline_bridge::Result<TransportConn>;
    async fn shutdown(&self) -> agentline_bridge::Result<()>;
}

// ── Multi-connection accept loop ───────────────────────────────────────

static CONN_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Spawn a thread that accepts connections from `listener` and serves each
/// one. ACP connections go through `serve_acp`; Files connections go through
/// the file browser handler.
pub fn spawn_transport(
    bridge: Bridge,
    listener: Arc<dyn TransportListener>,
    token: Option<String>,
    cwd: PathBuf,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("transport".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to create transport runtime");
            let local = tokio::task::LocalSet::new();
            rt.block_on(local.run_until(async move {
                loop {
                    match listener.accept().await {
                        Ok(conn) => {
                            let id = CONN_COUNTER.fetch_add(1, Ordering::Relaxed);
                            let bridge = bridge.clone();
                            let token = token.clone();
                            let cwd = cwd.clone();
                            tokio::task::spawn_local(async move {
                                match conn.protocol {
                                    Protocol::Acp => {
                                        handle_acp_connection(bridge, id, conn, token.as_deref())
                                            .await;
                                    }
                                    Protocol::Files => {
                                        files::handle_files_connection(
                                            cwd,
                                            id,
                                            conn,
                                            token.as_deref(),
                                        )
                                        .await;
                                    }
                                }
                            });
                        }
                        Err(e) => {
                            tracing::error!(error=%e, "transport accept failed; stopping listener");
                            break;
                        }
                    }
                }
                let _ = listener.shutdown().await;
            }));
        })
        .expect("failed to spawn transport thread")
}

async fn handle_acp_connection(
    bridge: Bridge,
    conn_id: u64,
    conn: TransportConn,
    token: Option<&str>,
) {
    let source_id = format!("acp:{conn_id}");

    let conn = if let Some(token) = token {
        match auth::wrap_authenticated(token, conn).await {
            Ok(c) => {
                tracing::info!(source=%source_id, "ACP transport connection authenticated");
                c
            }
            Err(e) => {
                tracing::warn!(source=%source_id, error=%e, "ACP auth rejected");
                return;
            }
        }
    } else {
        tracing::info!(source=%source_id, "ACP transport connection accepted (no auth)");
        conn
    };

    let (source, out_rx) = AcpSource::with_id(source_id.clone());
    bridge.router().register_source(source.clone());

    let result = serve_acp(bridge.clone(), source, out_rx, conn.read, conn.write).await;

    bridge.router().unregister_source(&source_id);
    match result {
        Ok(()) => tracing::info!(source=%source_id, "ACP transport connection closed"),
        Err(e) => tracing::error!(source=%source_id, error=%e, "ACP transport connection error"),
    }
}
