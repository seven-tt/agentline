//! Generic ACP (Agent Client Protocol) transport for agentline.
//!
//! Knows nothing about any specific coding agent. Give it a command + args
//! that speaks ACP over stdio (e.g. `npx @some/acp-agent`), and it gives you
//! an `agentline_bridge::AgentBackend` you can hand to a Bridge.
//!
//! For Claude Code specifically, prefer the `agentline-agent-claude-code`
//! crate — it adds the env scrubbing and `~/.claude/settings.json` plumbing
//! that claude-code-acp expects.
//!
//! Architecture (modeled after acp-cli's bridge):
//! - Spawns the agent child process directly so the caller can control
//!   env / args / cwd at construction time.
//! - Drives `agent_client_protocol::ClientSideConnection` over the child's
//!   stdio. ACP futures are `!Send`, so the bridge thread runs in
//!   `spawn_blocking` + `LocalSet`.
//! - Communicates with the outer (Send) `AgentBackend` via `mpsc` commands.

pub mod client_impl;
pub mod error;
pub mod mapping;

pub use error::{Error, Result};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::rc::Rc;
use std::sync::Arc;

use agent_client_protocol::{self as acp, Agent as _};
use agentline_bridge::{
    AgentBackend, AgentUpdate, Error as CoreError, Result as CoreResult, SessionId,
};
use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::client_impl::{BridgedClient, Routing};

#[derive(Debug, Clone)]
pub struct AcpBackendConfig {
    pub command: String,
    pub args: Vec<String>,
    /// Extra env vars to set on the child (applied after `remove_env`).
    pub extra_env: Vec<(String, String)>,
    /// Env vars to **strip** from the child's environment. Useful when the
    /// agent reacts to specific parent-process flags (e.g. nested-session
    /// detection). Empty by default.
    pub remove_env: Vec<String>,
    /// If set, the child PID is written to this file on spawn and removed on
    /// shutdown. The tray uses it to reap orphaned agent trees.
    pub pid_file: Option<PathBuf>,
}

impl AcpBackendConfig {
    pub fn new(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: command.into(),
            args,
            extra_env: Vec::new(),
            remove_env: Vec::new(),
            pid_file: None,
        }
    }
}

enum AcpCmd {
    NewSession {
        cwd: PathBuf,
        reply: oneshot::Sender<Result<SessionId>>,
    },
    Prompt {
        sid: SessionId,
        text: String,
        update_tx: mpsc::UnboundedSender<AgentUpdate>,
        ack: oneshot::Sender<Result<()>>,
    },
    Cancel {
        sid: SessionId,
        reply: oneshot::Sender<Result<()>>,
    },
    CloseSession {
        sid: SessionId,
        reply: oneshot::Sender<Result<()>>,
    },
}

pub struct AcpBackend {
    cmd_tx: mpsc::Sender<AcpCmd>,
    pending_perms: Arc<tokio::sync::Mutex<HashMap<String, oneshot::Sender<bool>>>>,
    /// Shared with BridgedClient so elicitation responses can be routed back
    /// without going through the cmd channel.
    pending_elicits: Arc<tokio::sync::Mutex<HashMap<String, oneshot::Sender<serde_json::Value>>>>,
    /// Process-group id of the spawned agent (== its pid). 0 until spawned.
    /// Used to SIGKILL the whole agent process tree on shutdown.
    child_pgid: Arc<std::sync::atomic::AtomicI32>,
    pid_file: Option<PathBuf>,
    _bg: JoinHandle<()>,
}

impl AcpBackend {
    pub async fn spawn(cfg: AcpBackendConfig) -> Result<Self> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<AcpCmd>(32);
        let pending_perms: Arc<tokio::sync::Mutex<HashMap<String, oneshot::Sender<bool>>>> =
            Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let pending_elicits: Arc<
            tokio::sync::Mutex<HashMap<String, oneshot::Sender<serde_json::Value>>>,
        > = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let pending_perms_for_bg = pending_perms.clone();
        let pending_elicits_for_bg = pending_elicits.clone();
        let child_pgid = Arc::new(std::sync::atomic::AtomicI32::new(0));
        let child_pgid_for_bg = child_pgid.clone();
        let pid_file = cfg.pid_file.clone();

        let bg = tokio::task::spawn_blocking(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!(error=%e, "failed to build runtime for ACP bridge");
                    return;
                }
            };
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                if let Err(e) = bridge_main(
                    cfg,
                    cmd_rx,
                    pending_perms_for_bg,
                    pending_elicits_for_bg,
                    child_pgid_for_bg,
                )
                .await
                {
                    tracing::error!(error=%e, "ACP bridge exited");
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

    /// Kill the entire agent process tree (the wrapper, the node process, and
    /// any shell/command it spawned). Safe to call multiple times.
    pub fn kill_agent_tree(&self) {
        let id = self.child_pgid.load(std::sync::atomic::Ordering::SeqCst);
        // Session kill first: catches descendants (`npm exec`, `node`, shells)
        // even after they `setpgid` into their own process group. The direct
        // process-group kill is a belt-and-suspenders fallback.
        kill_session(id);
        kill_process_group(id);
    }

    async fn send_cmd(&self, cmd: AcpCmd) -> Result<()> {
        self.cmd_tx.send(cmd).await.map_err(|_| Error::NotRunning)
    }
}

#[async_trait]
impl AgentBackend for AcpBackend {
    async fn new_session(&self, cwd: &Path) -> CoreResult<SessionId> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(AcpCmd::NewSession {
            cwd: cwd.to_path_buf(),
            reply: tx,
        })
        .await
        .map_err(|e| CoreError::agent(e.to_string()))?;
        rx.await
            .map_err(|_| CoreError::agent("ACP bridge dropped reply"))?
            .map_err(|e| CoreError::agent(e.to_string()))
    }

    async fn prompt<'a>(
        &'a self,
        sid: &'a SessionId,
        text: &'a str,
    ) -> CoreResult<BoxStream<'a, AgentUpdate>> {
        let (update_tx, update_rx) = mpsc::unbounded_channel();
        let (ack_tx, ack_rx) = oneshot::channel();
        self.send_cmd(AcpCmd::Prompt {
            sid: sid.clone(),
            text: text.to_string(),
            update_tx,
            ack: ack_tx,
        })
        .await
        .map_err(|e| CoreError::agent(e.to_string()))?;
        ack_rx
            .await
            .map_err(|_| CoreError::agent("ACP bridge dropped reply"))?
            .map_err(|e| CoreError::agent(e.to_string()))?;
        Ok(tokio_stream::wrappers::UnboundedReceiverStream::new(update_rx).boxed())
    }

    async fn cancel(&self, sid: &SessionId) -> CoreResult<()> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(AcpCmd::Cancel {
            sid: sid.clone(),
            reply: tx,
        })
        .await
        .map_err(|e| CoreError::agent(e.to_string()))?;
        rx.await
            .map_err(|_| CoreError::agent("ACP bridge dropped reply"))?
            .map_err(|e| CoreError::agent(e.to_string()))
    }

    async fn answer_permission(
        &self,
        _sid: &SessionId,
        request_id: &str,
        allow: bool,
    ) -> CoreResult<()> {
        // Bypass the cmd channel — the bridge thread is usually blocked in
        // `conn.prompt(...)` while a permission request is outstanding, so a
        // command-channel round-trip would deadlock. The pending oneshot is
        // shared between the bridge's `BridgedClient` and `AcpBackend`, so we
        // can fulfill it directly from any (Send) caller.
        let pending = self.pending_perms.lock().await.remove(request_id);
        match pending {
            Some(tx) => {
                let _ = tx.send(allow);
                Ok(())
            }
            None => Err(CoreError::agent(format!(
                "no pending permission with id {request_id}"
            ))),
        }
    }

    async fn close_session(&self, sid: SessionId) -> CoreResult<()> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(AcpCmd::CloseSession { sid, reply: tx })
            .await
            .map_err(|e| CoreError::agent(e.to_string()))?;
        rx.await
            .map_err(|_| CoreError::agent("ACP bridge dropped reply"))?
            .map_err(|e| CoreError::agent(e.to_string()))
    }

    async fn answer_elicitation(
        &self,
        elicit_id: &str,
        response: serde_json::Value,
    ) -> CoreResult<()> {
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

/// Spawn the agent child process applying `remove_env` / `extra_env` from `cfg`.
fn spawn_agent(cfg: &AcpBackendConfig) -> Result<tokio::process::Child> {
    let mut cmd = Command::new(&cfg.command);
    cmd.args(&cfg.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);

    // Put the agent in its own **session** (and thus a new process group) with
    // the child as leader, so `sid == pgid == child pid`. A new *group* alone is
    // not enough: `npm exec` calls `setpgid` to move itself and the `node
    // claude-code-acp` it spawns into a *fresh* process group, escaping the
    // group we tracked — which is why those processes used to survive shutdown
    // as orphans (reparented to launchd). `setpgid` cannot escape the session,
    // so at shutdown we kill by session id (see `kill_session`) to reliably
    // reap the entire tree. `kill_on_drop` only reaps the direct child.
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NEW_PROCESS_GROUP (0x200) lets us treat the child's pid as the
        // process-group id and terminate the whole tree on shutdown.
        cmd.creation_flags(0x00000200);
    }

    for k in &cfg.remove_env {
        cmd.env_remove(k);
    }

    // Always inject LAN exclusions so child agents that call local services
    // (e.g. Kimi's MCP server on 192.168.x.x) bypass the global proxy.
    // We do this *before* extra_env so the caller can override if needed.
    let no_proxy = agentline_bridge::proxy::build_no_proxy();
    cmd.env("NO_PROXY", &no_proxy).env("no_proxy", &no_proxy);

    for (k, v) in &cfg.extra_env {
        cmd.env(k, v);
    }

    cmd.spawn()
        .map_err(|e| Error::other(format!("spawn `{}`: {e}", cfg.command)))
}

async fn bridge_main(
    cfg: AcpBackendConfig,
    mut cmd_rx: mpsc::Receiver<AcpCmd>,
    pending_perms: Arc<tokio::sync::Mutex<HashMap<String, oneshot::Sender<bool>>>>,
    pending_elicits: Arc<tokio::sync::Mutex<HashMap<String, oneshot::Sender<serde_json::Value>>>>,
    child_pgid: Arc<std::sync::atomic::AtomicI32>,
) -> Result<()> {
    const MAX_RESPAWNS: u32 = 5;
    const RESPAWN_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(2);
    let mut respawn_count: u32 = 0;

    'outer: loop {
        tracing::debug!(cmd = %cfg.command, args = ?cfg.args, "spawning ACP agent");
        let mut child = spawn_agent(&cfg)?;
        if let Some(pid) = child.id() {
            child_pgid.store(pid as i32, std::sync::atomic::Ordering::SeqCst);
            if let Some(ref pf) = cfg.pid_file {
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

        // Clear stale pending requests from previous incarnation.
        pending_perms.lock().await.clear();
        pending_elicits.lock().await.clear();

        let routing = Routing::with_pending(pending_perms.clone(), pending_elicits.clone());
        let session_streams = routing.session_streams.clone();
        let client = BridgedClient::new(routing);

        let (conn, io_task) =
            acp::ClientSideConnection::new(client, stdin.compat_write(), stdout.compat(), |fut| {
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
            .map_err(|e| Error::protocol(format!("initialize: {e}")))?;

        if !init_resp.auth_methods.is_empty() {
            let names: Vec<String> = init_resp
                .auth_methods
                .iter()
                .map(|m| m.id().to_string())
                .collect();
            tracing::warn!(
                ?names,
                "agent advertises auth methods — ensure required out-of-band login is done before session/new"
            );
        }

        let mut sessions: HashMap<SessionId, acp::SessionId> = HashMap::new();

        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    let Some(cmd) = cmd else { break 'outer; };
                    match cmd {
                        AcpCmd::NewSession { cwd, reply } => {
                            let req = acp::NewSessionRequest::new(cwd);
                            match conn.as_ref().new_session(req).await {
                                Ok(resp) => {
                                    let acp_sid = resp.session_id;
                                    let core_sid = SessionId::new(acp_sid.0.to_string());
                                    sessions.insert(core_sid.clone(), acp_sid);
                                    let _ = reply.send(Ok(core_sid));
                                }
                                Err(e) => {
                                    let _ = reply.send(Err(Error::other(format!("new_session: {e}"))));
                                }
                            }
                        }
                        AcpCmd::Prompt {
                            sid,
                            text,
                            update_tx,
                            ack,
                        } => {
                            let acp_sid = match sessions.get(&sid) {
                                Some(s) => s.clone(),
                                None => {
                                    let _ = ack.send(Err(Error::SessionNotFound(sid.0)));
                                    continue;
                                }
                            };

                            session_streams
                                .lock()
                                .await
                                .insert(acp_sid.clone(), update_tx.clone());

                            let _ = ack.send(Ok(()));

                            let conn_for_task = conn.clone();
                            let session_streams_for_task = session_streams.clone();
                            let acp_sid_for_task = acp_sid.clone();
                            tokio::task::spawn_local(async move {
                                let prompt_req = acp::PromptRequest::new(
                                    acp_sid_for_task.clone(),
                                    vec![acp::ContentBlock::Text(acp::TextContent::new(text))],
                                );
                                match conn_for_task.as_ref().prompt(prompt_req).await {
                                    Ok(_resp) => {
                                        let _ = update_tx.send(AgentUpdate::Done);
                                    }
                                    Err(e) => {
                                        let _ = update_tx.send(AgentUpdate::Error(e.to_string()));
                                    }
                                }
                                session_streams_for_task
                                    .lock()
                                    .await
                                    .remove(&acp_sid_for_task);
                            });
                        }
                        AcpCmd::Cancel { sid, reply } => {
                            if let Some(acp_sid) = sessions.get(&sid).cloned() {
                                let conn_for_task = conn.clone();
                                tokio::task::spawn_local(async move {
                                    let _ = conn_for_task
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
                                    Ok(Ok(_)) => {
                                        tracing::debug!("ACP session/close succeeded");
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
                    break; // inner loop → respawn
                }
            }
        }

        // Cleanup the dead child before respawning.
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

    // Final cleanup on normal shutdown.
    let id = child_pgid.load(std::sync::atomic::Ordering::SeqCst);
    kill_session(id);
    kill_process_group(id);
    if let Some(ref pf) = cfg.pid_file {
        let _ = std::fs::remove_file(pf);
    }
    Ok(())
}

pub use agentline_bridge::process::cleanup_orphaned_agent;
use agentline_bridge::process::{kill_process_group, kill_session, write_pid_file};

impl Drop for AcpBackend {
    fn drop(&mut self) {
        self.kill_agent_tree();
        if let Some(ref pf) = self.pid_file {
            let _ = std::fs::remove_file(pf);
        }
    }
}

#[cfg(test)]
mod proxy_env_tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    /// Run a shell command through the real `spawn_agent` path and return its stdout.
    async fn spawn_and_capture(shell_cmd: &str) -> String {
        let cfg = AcpBackendConfig::new("/bin/sh", vec!["-c".into(), shell_cmd.into()]);
        let mut child = spawn_agent(&cfg).expect("spawn child");
        let mut out = String::new();
        child
            .stdout
            .take()
            .expect("piped stdout")
            .read_to_string(&mut out)
            .await
            .expect("read stdout");
        let _ = child.wait().await;
        out
    }

    /// The direct child must see the `NO_PROXY` we inject (with the LAN ranges),
    /// so LAN/local services bypass the global proxy.
    #[cfg(unix)]
    #[tokio::test]
    async fn direct_child_inherits_injected_no_proxy() {
        let out = spawn_and_capture(r#"printf '%s' "$NO_PROXY""#).await;
        assert!(
            out.contains("192.168.0.0/16"),
            "direct child NO_PROXY missing LAN range, got: {out:?}"
        );
    }

    /// The agent runs git inside a *nested* shell (its bash tool). A normal
    /// grandchild inherits the parent env, so NO_PROXY must reach it too — if it
    /// doesn't, the regression is the agent scrubbing env, not our injection.
    #[cfg(unix)]
    #[tokio::test]
    async fn nested_shell_grandchild_inherits_no_proxy() {
        let out = spawn_and_capture(r#"/bin/sh -c 'printf "%s" "$NO_PROXY"'"#).await;
        assert!(
            out.contains("192.168.0.0/16"),
            "nested-shell grandchild NO_PROXY missing LAN range, got: {out:?}"
        );
    }

    /// `kill_process_group` must take down the whole tree (the direct shell *and*
    /// the grandchildren it backgrounded), not just the direct child — that's the
    /// fallback reaper for orphaned `node claude-code-acp` processes.
    #[cfg(unix)]
    #[tokio::test]
    async fn kills_whole_process_group() {
        use std::time::Duration;

        // sh stays in the foreground `sleep`, with another `sleep` backgrounded —
        // both land in the new group/session created by `setsid` in `spawn_agent`.
        let cfg =
            AcpBackendConfig::new("/bin/sh", vec!["-c".into(), "sleep 300 & sleep 300".into()]);
        let mut child = spawn_agent(&cfg).expect("spawn");
        let pid = child.id().expect("child pid") as i32;

        tokio::time::sleep(Duration::from_millis(300)).await;
        // Sanity: the group is alive before we kill it.
        assert_eq!(
            unsafe { libc::kill(-pid, 0) },
            0,
            "process group should be alive before kill"
        );

        kill_process_group(pid);
        let _ = child.wait().await; // reap the group leader
        tokio::time::sleep(Duration::from_millis(500)).await; // let init reap the rest

        // No process should remain in the group (ESRCH).
        assert_ne!(
            unsafe { libc::kill(-pid, 0) },
            0,
            "process group {pid} still alive after kill_process_group"
        );
    }

    /// `kill_session` reaps the agent's whole session — the real fix for
    /// `npm exec` / `node` escaping the process group via `setpgid`. Since
    /// `spawn_agent` puts the child in its own session (`sid == pid`), killing
    /// that session must take down the foreground and backgrounded children.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn kills_whole_session() {
        use std::time::Duration;

        let cfg =
            AcpBackendConfig::new("/bin/sh", vec!["-c".into(), "sleep 300 & sleep 300".into()]);
        let mut child = spawn_agent(&cfg).expect("spawn");
        let pid = child.id().expect("child pid") as i32;

        tokio::time::sleep(Duration::from_millis(300)).await;
        // The child is its own session leader (setsid → sid == pid).
        assert_eq!(
            unsafe { libc::getsid(pid) },
            pid,
            "spawn_agent should put the child in its own session"
        );

        kill_session(pid);
        let _ = child.wait().await;
        tokio::time::sleep(Duration::from_millis(500)).await;

        assert_ne!(
            unsafe { libc::kill(-pid, 0) },
            0,
            "session {pid} still alive after kill_session"
        );
    }
}
