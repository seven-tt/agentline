//! ACP driver: the single loop that drives any ACP-speaking agent backend.
//!
//! Defines:
//! - [`ToolCallParser`]: per-agent hook for normalising quirky tool-call shapes.
//! - [`AcpCodec`]: thin trait each agent implements to supply spawn params.
//! - [`AcpBackend`]: `AgentBackend` implementation driven by `bridge_main`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use agent_client_protocol::{self as acp, Agent as _};
use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::acp_client::{BridgedClient, Routing};
use crate::process::{kill_process_group, kill_session, write_pid_file};
use crate::transport::SpawnSpec;
use crate::{AgentBackend, AgentSessionId, AgentUpdate, Error, Result};

/// Per-agent hook for normalising non-standard tool-call shapes.
///
/// The generic (spec-faithful) parse runs first; only when it returns `Other`
/// does the agent's parser get a chance — keeping the driver agent-agnostic.
pub trait ToolCallParser: std::fmt::Debug + Send + Sync {
    fn refine_kind(&self, _tool_call: &acp::ToolCallUpdate) -> Option<crate::ToolKind> {
        None
    }
    fn refine_what(&self, _tool_call: &acp::ToolCallUpdate) -> Option<String> {
        None
    }
    /// Called on every ToolCall/ToolCallUpdate from session_notification.
    /// Parsers can cache data for later use in `enrich_permission`.
    fn observe(&self, _tool_call: &acp::ToolCallUpdate) {}
    /// Called before extracting what/kind from a permission request's tool_call.
    /// Parsers can fill in missing fields from previously observed data.
    fn enrich_permission(&self, tool_call: acp::ToolCallUpdate) -> acp::ToolCallUpdate {
        tool_call
    }
}

/// What an agent backend must supply so the driver can spawn and drive it.
pub trait AcpCodec: Send + Sync + 'static {
    fn spawn_spec(&self) -> SpawnSpec;
    fn pid_file(&self) -> Option<PathBuf> {
        None
    }
    fn tool_call_parser(&self) -> Option<Arc<dyn ToolCallParser>> {
        None
    }
    fn mcp_servers(&self) -> Vec<acp::McpServer> {
        vec![]
    }
}

// ─── internal command channel ────────────────────────────────────────────────

enum AcpCmd {
    NewSession {
        cwd: PathBuf,
        reply: oneshot::Sender<Result<AgentSessionId>>,
    },
    Prompt {
        sid: AgentSessionId,
        content: Vec<acp::ContentBlock>,
        update_tx: mpsc::UnboundedSender<AgentUpdate>,
        ack: oneshot::Sender<Result<()>>,
    },
    Cancel {
        sid: AgentSessionId,
        reply: oneshot::Sender<Result<()>>,
    },
    CloseSession {
        sid: AgentSessionId,
        reply: oneshot::Sender<Result<()>>,
    },
}

// ─── AcpBackend ──────────────────────────────────────────────────────────────

pub struct AcpBackend {
    cmd_tx: mpsc::Sender<AcpCmd>,
    pending_perms: Arc<tokio::sync::Mutex<HashMap<String, oneshot::Sender<bool>>>>,
    pending_elicits: Arc<tokio::sync::Mutex<HashMap<String, oneshot::Sender<serde_json::Value>>>>,
    child_pgid: Arc<std::sync::atomic::AtomicI32>,
    pid_file: Option<PathBuf>,
    _bg: JoinHandle<()>,
}

impl AcpBackend {
    /// Spawn an agent process via `codec` and return a ready backend.
    pub async fn spawn(codec: impl AcpCodec + 'static) -> Result<Self> {
        Self::spawn_dyn(Arc::new(codec)).await
    }

    async fn spawn_dyn(codec: Arc<dyn AcpCodec>) -> Result<Self> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<AcpCmd>(32);
        let pending_perms: Arc<tokio::sync::Mutex<HashMap<String, oneshot::Sender<bool>>>> =
            Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let pending_elicits: Arc<
            tokio::sync::Mutex<HashMap<String, oneshot::Sender<serde_json::Value>>>,
        > = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let pending_perms_bg = pending_perms.clone();
        let pending_elicits_bg = pending_elicits.clone();
        let child_pgid = Arc::new(std::sync::atomic::AtomicI32::new(0));
        let child_pgid_bg = child_pgid.clone();
        let pid_file = codec.pid_file();

        let bg = tokio::task::spawn_blocking(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!(error=%e, "failed to build ACP driver runtime");
                    return;
                }
            };
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                if let Err(e) = bridge_main(
                    codec,
                    cmd_rx,
                    pending_perms_bg,
                    pending_elicits_bg,
                    child_pgid_bg,
                )
                .await
                {
                    tracing::error!(error=%e, "ACP driver exited");
                }
            });
        });

        Ok(Self {
            cmd_tx,
            pending_perms,
            pending_elicits,
            child_pgid,
            pid_file,
            _bg: bg,
        })
    }

    pub fn kill_agent_tree(&self) {
        let id = self.child_pgid.load(std::sync::atomic::Ordering::SeqCst);
        kill_session(id);
        kill_process_group(id);
    }

    async fn send_cmd(&self, cmd: AcpCmd) -> Result<()> {
        self.cmd_tx
            .send(cmd)
            .await
            .map_err(|_| Error::other("agent not running"))
    }
}

#[async_trait]
impl AgentBackend for AcpBackend {
    async fn new_session(&self, cwd: &std::path::Path) -> Result<AgentSessionId> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(AcpCmd::NewSession {
            cwd: cwd.to_path_buf(),
            reply: tx,
        })
        .await?;
        rx.await
            .map_err(|_| Error::other("ACP driver dropped reply"))?
    }

    async fn prompt<'a>(
        &'a self,
        sid: &'a AgentSessionId,
        content: &'a [acp::ContentBlock],
    ) -> Result<BoxStream<'a, AgentUpdate>> {
        let (update_tx, update_rx) = mpsc::unbounded_channel();
        let (ack_tx, ack_rx) = oneshot::channel();
        self.send_cmd(AcpCmd::Prompt {
            sid: sid.clone(),
            content: content.to_vec(),
            update_tx,
            ack: ack_tx,
        })
        .await?;
        ack_rx
            .await
            .map_err(|_| Error::other("ACP driver dropped reply"))??;
        Ok(tokio_stream::wrappers::UnboundedReceiverStream::new(update_rx).boxed())
    }

    async fn cancel(&self, sid: &AgentSessionId) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(AcpCmd::Cancel {
            sid: sid.clone(),
            reply: tx,
        })
        .await?;
        rx.await
            .map_err(|_| Error::other("ACP driver dropped reply"))?
    }

    async fn respond_permission(
        &self,
        _sid: &AgentSessionId,
        request_id: &str,
        allow: bool,
    ) -> Result<()> {
        let pending = self.pending_perms.lock().await.remove(request_id);
        match pending {
            Some(tx) => {
                let _ = tx.send(allow);
                Ok(())
            }
            None => Err(Error::other(format!(
                "no pending permission with id {request_id}"
            ))),
        }
    }

    async fn close_session(&self, sid: AgentSessionId) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(AcpCmd::CloseSession { sid, reply: tx })
            .await?;
        rx.await
            .map_err(|_| Error::other("ACP driver dropped reply"))?
    }

    async fn respond_elicitation(
        &self,
        elicit_id: &str,
        response: serde_json::Value,
    ) -> Result<()> {
        let pending = self.pending_elicits.lock().await.remove(elicit_id);
        match pending {
            Some(tx) => {
                let _ = tx.send(response);
                Ok(())
            }
            None => {
                tracing::warn!(elicit_id, "no pending elicitation with this id");
                Ok(())
            }
        }
    }

    async fn shutdown(&self) {
        self.kill_agent_tree();
    }
}

impl Drop for AcpBackend {
    fn drop(&mut self) {
        self.kill_agent_tree();
        if let Some(ref pf) = self.pid_file {
            let _ = std::fs::remove_file(pf);
        }
    }
}

// ─── bridge_main: the single ACP driver loop ─────────────────────────────────

async fn bridge_main(
    codec: Arc<dyn AcpCodec>,
    mut cmd_rx: mpsc::Receiver<AcpCmd>,
    pending_perms: Arc<tokio::sync::Mutex<HashMap<String, oneshot::Sender<bool>>>>,
    pending_elicits: Arc<tokio::sync::Mutex<HashMap<String, oneshot::Sender<serde_json::Value>>>>,
    child_pgid: Arc<std::sync::atomic::AtomicI32>,
) -> Result<()> {
    const MAX_RESPAWNS: u32 = 5;
    const RESPAWN_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(2);
    let mut respawn_count: u32 = 0;

    let pid_file = codec.pid_file();

    'outer: loop {
        let spec = codec.spawn_spec();
        let agent_name = spec.command.clone();
        tracing::debug!(cmd = %agent_name, args = ?spec.args, "spawning ACP agent");

        let mut child = crate::transport::spawn(&spec)
            .map_err(|e| Error::other(format!("spawn `{agent_name}`: {e}")))?;

        if let Some(pid) = child.id() {
            child_pgid.store(pid as i32, std::sync::atomic::Ordering::SeqCst);
            if let Some(ref pf) = pid_file {
                write_pid_file(pf, pid);
            }
        }

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::other("child has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::other("child has no stdout"))?;

        pending_perms.lock().await.clear();
        pending_elicits.lock().await.clear();

        let routing = Routing::with_pending(pending_perms.clone(), pending_elicits.clone());
        let session_streams = routing.session_streams.clone();
        let client = BridgedClient::new(routing, codec.tool_call_parser());

        let logged_in = crate::raw_log::LineLog::writer(stdin.compat_write(), agent_name.clone());
        let logged_out = crate::raw_log::LineLog::reader(stdout.compat(), agent_name);
        let (conn, io_task) =
            acp::ClientSideConnection::new(client, logged_in, logged_out, |fut| {
                tokio::task::spawn_local(fut);
            });
        let conn = Rc::new(conn);

        let (io_dead_tx, mut io_dead_rx) = oneshot::channel::<()>();
        tokio::task::spawn_local(async move {
            if let Err(e) = io_task.await {
                tracing::error!(error=%e, "ACP I/O loop ended");
            }
            let _ = io_dead_tx.send(());
        });

        let init_resp = conn
            .as_ref()
            .initialize(
                acp::InitializeRequest::new(acp::ProtocolVersion::V1).client_info(
                    acp::Implementation::new("agentline", env!("CARGO_PKG_VERSION")),
                ),
            )
            .await
            .map_err(|e| Error::other(format!("initialize: {e}")))?;

        if !init_resp.auth_methods.is_empty() {
            let names: Vec<String> = init_resp
                .auth_methods
                .iter()
                .map(|m| m.id().to_string())
                .collect();
            tracing::warn!(
                ?names,
                "agent advertises auth methods — complete the agent's own login out of band \
                 (e.g. `kimi login`)"
            );
        }

        let mut sessions: HashMap<AgentSessionId, acp::SessionId> = HashMap::new();

        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    let Some(cmd) = cmd else { break 'outer; };
                    match cmd {
                        AcpCmd::NewSession { cwd, reply } => {
                            let req = acp::NewSessionRequest::new(cwd)
                                .mcp_servers(codec.mcp_servers());
                            match conn.as_ref().new_session(req).await {
                                Ok(resp) => {
                                    let acp_sid = resp.session_id;
                                    let core_sid = AgentSessionId::new(acp_sid.0.to_string());
                                    sessions.insert(core_sid.clone(), acp_sid);
                                    let _ = reply.send(Ok(core_sid));
                                }
                                Err(e) => {
                                    let _ = reply.send(Err(Error::other(format!("new_session: {e}"))));
                                }
                            }
                        }
                        AcpCmd::Prompt { sid, content, update_tx, ack } => {
                            let acp_sid = match sessions.get(&sid) {
                                Some(s) => s.clone(),
                                None => {
                                    let _ = ack.send(Err(Error::other(format!(
                                        "session not found: {}", sid.0
                                    ))));
                                    continue;
                                }
                            };
                            session_streams
                                .lock()
                                .await
                                .insert(acp_sid.clone(), update_tx.clone());
                            let _ = ack.send(Ok(()));
                            let conn2 = conn.clone();
                            let streams2 = session_streams.clone();
                            let acp_sid2 = acp_sid.clone();
                            tokio::task::spawn_local(async move {
                                let req = acp::PromptRequest::new(acp_sid2.clone(), content);
                                match conn2.as_ref().prompt(req).await {
                                    Ok(_) => {
                                        let _ = update_tx.send(AgentUpdate::Done);
                                    }
                                    Err(e) => {
                                        let _ = update_tx.send(AgentUpdate::Error(e.to_string()));
                                    }
                                }
                                streams2.lock().await.remove(&acp_sid2);
                            });
                        }
                        AcpCmd::Cancel { sid, reply } => {
                            if let Some(acp_sid) = sessions.get(&sid).cloned() {
                                let conn2 = conn.clone();
                                tokio::task::spawn_local(async move {
                                    let _ = conn2
                                        .as_ref()
                                        .cancel(acp::CancelNotification::new(acp_sid))
                                        .await;
                                });
                            }
                            let _ = reply.send(Ok(()));
                        }
                        AcpCmd::CloseSession { sid, reply } => {
                            if let Some(acp_sid) = sessions.remove(&sid) {
                                let req = acp::CloseSessionRequest::new(acp_sid);
                                match tokio::time::timeout(
                                    std::time::Duration::from_secs(3),
                                    conn.as_ref().close_session(req),
                                )
                                .await
                                {
                                    Ok(Ok(_)) => {}
                                    Ok(Err(e)) if e.code == acp::ErrorCode::MethodNotFound => {
                                        tracing::info!(
                                            "agent does not support session/close, skipping"
                                        );
                                    }
                                    Ok(Err(e)) => {
                                        tracing::warn!(error=%e, "ACP session/close failed");
                                    }
                                    Err(_) => {
                                        tracing::warn!("ACP session/close timed out after 3s");
                                    }
                                }
                            }
                            let _ = reply.send(Ok(()));
                        }
                    }
                }
                _ = &mut io_dead_rx => {
                    tracing::warn!("ACP I/O transport died unexpectedly");
                    break;
                }
            }
        }

        let old_pid = child_pgid.load(std::sync::atomic::Ordering::SeqCst);
        kill_session(old_pid);
        kill_process_group(old_pid);
        let _ = child.start_kill();
        let _ = child.wait().await;

        respawn_count += 1;
        if respawn_count > MAX_RESPAWNS {
            tracing::error!(
                limit = MAX_RESPAWNS,
                "agent respawn limit reached, giving up"
            );
            break 'outer;
        }
        tracing::info!(attempt = respawn_count, "respawning agent after crash...");
        tokio::time::sleep(RESPAWN_COOLDOWN).await;
    }

    let id = child_pgid.load(std::sync::atomic::Ordering::SeqCst);
    kill_session(id);
    kill_process_group(id);
    if let Some(ref pf) = pid_file {
        let _ = std::fs::remove_file(pf);
    }
    Ok(())
}
