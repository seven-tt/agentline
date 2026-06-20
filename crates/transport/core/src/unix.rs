//! Unix domain socket transport.
//!
//! After accepting a connection, reads a 1-byte protocol tag:
//! - `0x01` → ACP
//! - `0x02` → Files

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::AsyncReadExt;
use tokio::net as tnet;
use tokio::sync::Notify;

use crate::{Protocol, TransportConn, TransportListener};

pub const TAG_ACP: u8 = 0x01;
pub const TAG_FILES: u8 = 0x02;

pub struct UnixSocketListener {
    listener: tnet::UnixListener,
    path: PathBuf,
    shutdown: Arc<Notify>,
}

impl UnixSocketListener {
    pub fn bind(path: impl AsRef<Path>) -> agentline_bridge::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if path.exists() {
            std::fs::remove_file(&path).ok();
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let listener = tnet::UnixListener::bind(&path)
            .map_err(|e| agentline_bridge::Error::other(format!("bind {}: {e}", path.display())))?;
        tracing::info!(path=%path.display(), "unix socket transport listening");
        Ok(Self {
            listener,
            path,
            shutdown: Arc::new(Notify::new()),
        })
    }
}

#[async_trait]
impl TransportListener for UnixSocketListener {
    async fn accept(&self) -> agentline_bridge::Result<TransportConn> {
        tokio::select! {
            result = self.listener.accept() => {
                let (mut stream, _addr) = result
                    .map_err(|e| agentline_bridge::Error::other(format!("unix accept: {e}")))?;
                let tag = stream.read_u8().await
                    .map_err(|e| agentline_bridge::Error::other(format!("unix read tag: {e}")))?;
                let protocol = match tag {
                    TAG_ACP => Protocol::Acp,
                    TAG_FILES => Protocol::Files,
                    other => {
                        return Err(agentline_bridge::Error::other(
                            format!("unknown protocol tag: 0x{other:02x}")
                        ));
                    }
                };
                let (read, write) = tokio::io::split(stream);
                Ok(TransportConn {
                    protocol,
                    read: Box::new(read),
                    write: Box::new(write),
                })
            }
            () = self.shutdown.notified() => {
                Err(agentline_bridge::Error::other("unix listener shutting down"))
            }
        }
    }

    async fn shutdown(&self) -> agentline_bridge::Result<()> {
        self.shutdown.notify_one();
        std::fs::remove_file(&self.path).ok();
        tracing::info!(path=%self.path.display(), "unix socket removed");
        Ok(())
    }
}
