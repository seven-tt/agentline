//! OpenAI Codex CLI agent backend for agentline.
//!
//! Spawns `codex app-server` as a subprocess (via the official
//! [`codex_app_server_sdk`]) and exposes it as an
//! [`AgentBackend`](agentline_bridge::AgentBackend).
//!
//! Codex doesn't speak ACP — it uses its own JSON-RPC "app-server" protocol.
//! That's why we depend on `codex-app-server-sdk` instead of reusing
//! `agentline-agent-acp`.
//!
//! # Prerequisites
//!
//! 1. Install the codex CLI:
//!    ```bash
//!    npm i -g @openai/codex     # or: brew install codex
//!    ```
//! 2. Authenticate once (`codex login`).
//!
//! # Permissions
//!
//! Codex governs side-effects via two orthogonal knobs: **`SandboxMode`**
//! (read-only / workspace-write / danger-full-access) and **`ApprovalMode`**
//! (never / on-request / on-failure / untrusted). The defaults here are
//! **WorkspaceWrite + Never** — matching the "yolo" experience the
//! claude-code backend has when `auto_approve_all` is on. Adjust via
//! [`CodexConfig::sandbox_mode`] / [`CodexConfig::approval_mode`] if you
//! want codex to ask before destructive operations.

pub mod config;
pub mod error;
pub mod mapping;
pub mod plugin;

pub use codex_app_server_sdk::api::{ApprovalMode, SandboxMode};
pub use error::{Error, Result};
pub use plugin::plugin;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};

use agentline_bridge::{
    AgentBackend, AgentSessionId, AgentUpdate, Error as CoreError, Result as CoreResult,
};
use async_trait::async_trait;
use codex_app_server_sdk::StdioConfig;
use codex_app_server_sdk::api::{Codex, Thread, ThreadOptions, TurnOptions};
use futures::stream::{BoxStream, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

#[derive(Debug, Clone)]
pub struct CodexConfig {
    /// Override the codex binary path (default: `codex` from PATH).
    pub command: Option<String>,
    /// Override the launcher args (default: `["app-server"]`).
    pub args: Option<Vec<String>>,
    /// Extra env vars set on the child.
    pub extra_env: Vec<(String, String)>,
    /// Sandbox mode for tool execution.
    pub sandbox_mode: SandboxMode,
    /// Approval mode for side-effecting operations.
    pub approval_mode: ApprovalMode,
    /// Don't refuse to run outside a git repo.
    pub skip_git_repo_check: bool,
    /// Optional model override (e.g. `"gpt-5-codex"`); codex uses its
    /// configured default if `None`.
    pub model: Option<String>,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            command: None,
            args: None,
            extra_env: Vec::new(),
            sandbox_mode: SandboxMode::WorkspaceWrite,
            approval_mode: ApprovalMode::Never,
            skip_git_repo_check: true,
            model: None,
        }
    }
}

enum CodexCmd {
    NewSession {
        cwd: PathBuf,
        reply: oneshot::Sender<Result<AgentSessionId>>,
    },
    Prompt {
        sid: AgentSessionId,
        text: String,
        update_tx: mpsc::UnboundedSender<AgentUpdate>,
        ack: oneshot::Sender<Result<()>>,
    },
    Cancel {
        // TODO: codex SDK's high-level streaming API has no in-flight cancel
        // hook yet; the bridge aborts the prompt task on its side and we ack OK.
        #[allow(dead_code)]
        sid: AgentSessionId,
        reply: oneshot::Sender<Result<()>>,
    },
    CloseSession {
        sid: AgentSessionId,
        reply: oneshot::Sender<Result<()>>,
    },
}

pub struct CodexBackend {
    cmd_tx: mpsc::Sender<CodexCmd>,
    child_pid: Arc<AtomicI32>,
    _bg: JoinHandle<()>,
}

impl CodexBackend {
    pub async fn spawn(cfg: CodexConfig) -> Result<Self> {
        let our_pid = std::process::id() as i32;
        let before = agentline_bridge::process::list_child_pids(our_pid);

        let stdio = build_stdio_config(&cfg);
        let codex = Codex::spawn_stdio(stdio)
            .await
            .map_err(|e| Error::sdk(format!("spawn codex app-server: {e}")))?;

        // Allow the child process tree to fully fork before scanning.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let new_pids = agentline_bridge::process::find_new_child_pids(our_pid, &before);
        let child_pid = Arc::new(AtomicI32::new(new_pids.first().copied().unwrap_or(0)));
        if child_pid.load(Ordering::SeqCst) > 0 {
            tracing::debug!(
                pid = child_pid.load(Ordering::SeqCst),
                "tracked codex child process"
            );
        } else {
            tracing::warn!("could not detect codex child PID; orphan cleanup may not work");
        }

        let (cmd_tx, cmd_rx) = mpsc::channel::<CodexCmd>(32);
        let cfg_for_bg = cfg.clone();
        let bg = tokio::spawn(async move {
            if let Err(e) = bridge_main(codex, cmd_rx, cfg_for_bg).await {
                tracing::error!(error=%e, "codex bridge exited");
            }
        });
        Ok(Self {
            cmd_tx,
            child_pid,
            _bg: bg,
        })
    }

    fn kill_child(&self) {
        let pid = self.child_pid.load(Ordering::SeqCst);
        if pid > 1 {
            agentline_bridge::process::kill_session(pid);
            agentline_bridge::process::kill_process_group(pid);
        }
    }

    async fn send_cmd(&self, cmd: CodexCmd) -> Result<()> {
        self.cmd_tx.send(cmd).await.map_err(|_| Error::NotRunning)
    }
}

impl Drop for CodexBackend {
    fn drop(&mut self) {
        self.kill_child();
    }
}

#[async_trait]
impl AgentBackend for CodexBackend {
    async fn new_session(&self, cwd: &Path) -> CoreResult<AgentSessionId> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(CodexCmd::NewSession {
            cwd: cwd.to_path_buf(),
            reply: tx,
        })
        .await
        .map_err(|e| CoreError::agent(e.to_string()))?;
        rx.await
            .map_err(|_| CoreError::agent("codex bridge dropped reply"))?
            .map_err(|e| CoreError::agent(e.to_string()))
    }

    async fn prompt<'a>(
        &'a self,
        sid: &'a AgentSessionId,
        content: &'a [agentline_bridge::types::ContentBlock],
    ) -> CoreResult<BoxStream<'a, AgentUpdate>> {
        let text = agentline_bridge::types::content_to_text(content);
        let (update_tx, update_rx) = mpsc::unbounded_channel();
        let (ack_tx, ack_rx) = oneshot::channel();
        self.send_cmd(CodexCmd::Prompt {
            sid: sid.clone(),
            text,
            update_tx,
            ack: ack_tx,
        })
        .await
        .map_err(|e| CoreError::agent(e.to_string()))?;
        ack_rx
            .await
            .map_err(|_| CoreError::agent("codex bridge dropped reply"))?
            .map_err(|e| CoreError::agent(e.to_string()))?;
        Ok(tokio_stream::wrappers::UnboundedReceiverStream::new(update_rx).boxed())
    }

    async fn cancel(&self, sid: &AgentSessionId) -> CoreResult<()> {
        // Codex's high-level streaming API doesn't expose an in-flight
        // cancel today; we ack OK and let the bridge abort its prompt task.
        let (tx, rx) = oneshot::channel();
        self.send_cmd(CodexCmd::Cancel {
            sid: sid.clone(),
            reply: tx,
        })
        .await
        .map_err(|e| CoreError::agent(e.to_string()))?;
        rx.await
            .map_err(|_| CoreError::agent("codex bridge dropped reply"))?
            .map_err(|e| CoreError::agent(e.to_string()))
    }

    async fn respond_permission(
        &self,
        _sid: &AgentSessionId,
        _request_id: &str,
        _allow: bool,
    ) -> CoreResult<()> {
        // Codex's high-level streaming API doesn't surface ServerRequest
        // permission asks. Configure approval_mode at session-start to
        // pre-decide. Treat as no-op so the bridge keeps moving.
        Ok(())
    }

    async fn close_session(&self, sid: AgentSessionId) -> CoreResult<()> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(CodexCmd::CloseSession { sid, reply: tx })
            .await
            .map_err(|e| CoreError::agent(e.to_string()))?;
        rx.await
            .map_err(|_| CoreError::agent("codex bridge dropped reply"))?
            .map_err(|e| CoreError::agent(e.to_string()))
    }

    async fn shutdown(&self) {
        self.kill_child();
    }
}

fn build_stdio_config(cfg: &CodexConfig) -> StdioConfig {
    let mut s = StdioConfig::default();
    if let Some(c) = &cfg.command {
        s.codex_binary = c.clone();
    }
    if let Some(a) = &cfg.args {
        s.args = a.clone();
    }
    // Inject LAN exclusions before extra_env so the caller can still override.
    let no_proxy = agentline_bridge::proxy::build_no_proxy();
    s.env.insert("NO_PROXY".to_string(), no_proxy.clone());
    s.env.insert("no_proxy".to_string(), no_proxy);
    for (k, v) in &cfg.extra_env {
        s.env.insert(k.clone(), v.clone());
    }
    s
}

fn build_thread_options(cfg: &CodexConfig, cwd: &Path) -> ThreadOptions {
    let mut b = ThreadOptions::builder()
        .sandbox_mode(cfg.sandbox_mode)
        .approval_policy(cfg.approval_mode)
        .working_directory(cwd.display().to_string());
    if cfg.skip_git_repo_check {
        b = b.skip_git_repo_check(true);
    }
    if let Some(m) = &cfg.model {
        b = b.model(m.clone());
    }
    b.build()
}

async fn bridge_main(
    codex: Codex,
    mut cmd_rx: mpsc::Receiver<CodexCmd>,
    cfg: CodexConfig,
) -> Result<()> {
    let mut threads: HashMap<AgentSessionId, Thread> = HashMap::new();
    let counter = AtomicU64::new(0);
    let (return_tx, mut return_rx) = mpsc::unbounded_channel::<(AgentSessionId, Thread)>();

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break; };
                match cmd {
                    CodexCmd::NewSession { cwd, reply } => {
                        let opts = build_thread_options(&cfg, &cwd);
                        let thread = codex.start_thread(opts);
                        let n = counter.fetch_add(1, Ordering::SeqCst);
                        let sid = AgentSessionId::new(format!("codex-{n}"));
                        threads.insert(sid.clone(), thread);
                        let _ = reply.send(Ok(sid));
                    }
                    CodexCmd::Prompt { sid, text, update_tx, ack } => {
                        let Some(mut thread) = threads.remove(&sid) else {
                            let _ = ack.send(Err(Error::SessionNotFound(sid.0)));
                            continue;
                        };
                        let _ = ack.send(Ok(()));
                        let return_tx = return_tx.clone();
                        let sid_for_task = sid.clone();
                        tokio::spawn(async move {
                            match thread.run_streamed(text, TurnOptions::default()).await {
                                Ok(mut stream) => {
                                    while let Some(event) = stream.next_event().await {
                                        match event {
                                            Ok(ev) => {
                                                for u in mapping::map_thread_event(ev) {
                                                    let _ = update_tx.send(u);
                                                }
                                            }
                                            Err(e) => {
                                                let _ = update_tx.send(
                                                    AgentUpdate::Error(format!("stream: {e}"))
                                                );
                                                break;
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    let _ = update_tx.send(
                                        AgentUpdate::Error(format!("run_streamed: {e}"))
                                    );
                                }
                            }
                            let _ = update_tx.send(AgentUpdate::Done);
                            let _ = return_tx.send((sid_for_task, thread));
                        });
                    }
                    CodexCmd::Cancel { sid: _, reply } => {
                        // No-op for now (see AgentBackend::cancel comment).
                        let _ = reply.send(Ok(()));
                    }
                    CodexCmd::CloseSession { sid, reply } => {
                        threads.remove(&sid);
                        let _ = reply.send(Ok(()));
                    }
                }
            }
            Some((sid, thread)) = return_rx.recv() => {
                // Put the thread back so the next prompt on the same session
                // reuses its conversation history.
                threads.insert(sid, thread);
            }
        }
    }
    Ok(())
}
