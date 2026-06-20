use crate::agent::{AgentBackend, AgentFactory, AgentInfo};
use crate::error::Result;
use crate::event::AgentEvent;
use crate::permission::{
    PendingPermissionRequest, PermissionDecision, PermissionPolicy, PermissionResponse,
};
use crate::registry::SessionRegistry;
use crate::router::SourceRouter;
use crate::session::{ChannelBinding, Session, gen_session_id};
use crate::state::{BridgeState, PendingElicit};
use crate::types::{AgentUpdate, ContentBlock, PeerRef, Project, SessionId, SessionInfo, ToolKind};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

// ── Configuration ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub default_cwd: PathBuf,
    pub typing_interval: Duration,
    pub session_idle_timeout: Duration,
    pub agent_name: String,
    pub projects: Vec<Project>,
    pub session_base_dir: Option<PathBuf>,
    pub registry: Option<Arc<SessionRegistry>>,
    /// Path to the TOML config file. When set, `/agent <name>` persists
    /// the new `agent.backend` value here.
    pub config_path: Option<PathBuf>,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            default_cwd: PathBuf::from("."),
            typing_interval: Duration::from_millis(5000),
            session_idle_timeout: Duration::from_secs(2 * 3600),
            agent_name: "agent".into(),
            projects: Vec::new(),
            session_base_dir: None,
            registry: None,
            config_path: None,
        }
    }
}

// ── Commands ───────────────────────────────────────────────────────────

pub(crate) enum BridgeCommand {
    // Public API
    NewSession {
        source_id: String,
        peer: PeerRef,
        reply: oneshot::Sender<Result<SessionId>>,
    },
    CloseSession {
        session_id: SessionId,
        reply: oneshot::Sender<Result<Option<String>>>,
    },
    Cancel {
        session_id: SessionId,
        reply: oneshot::Sender<Result<Option<String>>>,
    },
    SetMode {
        mode: String,
        session_id: Option<SessionId>,
        reply: oneshot::Sender<crate::types::ModeResult>,
    },
    SetCwd {
        path: PathBuf,
        session_id: Option<SessionId>,
        reply: oneshot::Sender<crate::types::CwdResult>,
    },
    GetCwdAndSubdirs {
        reply: oneshot::Sender<(PathBuf, Vec<PathBuf>)>,
    },
    Prompt {
        session_id: SessionId,
        content: Vec<ContentBlock>,
        reply: oneshot::Sender<Result<crate::types::PromptResult>>,
    },
    RespondPermission {
        response: PermissionResponse,
        session_id: SessionId,
        reply: oneshot::Sender<crate::types::PermissionResult>,
    },
    OverridePendingPermissionRequest {
        session_id: SessionId,
        reply: oneshot::Sender<bool>,
    },
    RespondElicitation {
        text: String,
        schema: Option<agent_client_protocol::ElicitationSchema>,
        session_id: SessionId,
        reply: oneshot::Sender<bool>,
    },
    SetProvider {
        name: String,
        reply: oneshot::Sender<Result<()>>,
    },
    ListProviders {
        reply: oneshot::Sender<Option<(String, Vec<AgentInfo>)>>,
    },
    SessionInfo {
        session_id: SessionId,
        reply: oneshot::Sender<Option<SessionInfo>>,
    },

    // Internal (used by relay/scheduler spawn tasks)
    InsertPendingPermissionRequest {
        session_id: SessionId,
        perm: PendingPermissionRequest,
    },
    InsertPendingElicit {
        session_id: SessionId,
        elicit: PendingElicit,
    },
    GetPermDecision {
        session_id: SessionId,
        tool_kind: ToolKind,
        what: String,
        reply: oneshot::Sender<PermissionDecision>,
    },
    SetRunningSession {
        session_id: SessionId,
    },
    ClearRunningSession,
    RemoveSession {
        session_id: SessionId,
    },
    PromptTaskDone,
    SetAgentFactory {
        factory: Arc<dyn AgentFactory>,
    },
    UpdateProjects {
        projects: Vec<Project>,
    },
}

// ── Bridge (public handle) ─────────────────────────────────────────────

#[derive(Clone)]
pub struct Bridge {
    tx: mpsc::Sender<BridgeCommand>,
    cfg: Arc<BridgeConfig>,
    router: Arc<SourceRouter>,
}

impl Bridge {
    pub fn from_router(
        router: SourceRouter,
        agent: Arc<dyn AgentBackend>,
        cfg: BridgeConfig,
        handler: Arc<dyn crate::source::InboundHandler>,
    ) -> (Self, JoinHandle<()>) {
        let (tx, rx) = mpsc::channel(256);
        let router = Arc::new(router);
        let cfg = Arc::new(cfg);

        let actor = BridgeActor::new(
            rx,
            tx.clone(),
            router.clone(),
            agent,
            (*cfg).clone(),
            handler.clone(),
        );
        let handle = Bridge { tx, cfg, router };
        let handle_for_loop = handle.clone();
        let actor_handle = tokio::spawn(async move {
            actor.run(handle_for_loop, handler).await;
        });
        (handle, actor_handle)
    }

    pub fn with_agent_factory(self, factory: Arc<dyn AgentFactory>) -> Self {
        let _ = self.tx.try_send(BridgeCommand::SetAgentFactory { factory });
        self
    }

    async fn send_cmd<T>(&self, f: impl FnOnce(oneshot::Sender<T>) -> BridgeCommand) -> T {
        let (tx, rx) = oneshot::channel();
        let cmd = f(tx);
        let _ = self.tx.send(cmd).await;
        rx.await.expect("actor dropped")
    }

    /// Explicitly create a new session bound to `(source_id, peer)`. The cwd is
    /// the bridge's current working directory (set via `set_cwd`). Returns the
    /// external [`SessionId`] the caller should use for subsequent operations.
    pub async fn new_session(&self, source_id: String, peer: PeerRef) -> Result<SessionId> {
        self.send_cmd(|reply| BridgeCommand::NewSession {
            source_id,
            peer,
            reply,
        })
        .await
    }

    pub async fn close_session(&self, session_id: &SessionId) -> Result<Option<String>> {
        self.send_cmd(|reply| BridgeCommand::CloseSession {
            session_id: session_id.clone(),
            reply,
        })
        .await
    }

    pub async fn cancel(&self, session_id: &SessionId) -> Result<Option<String>> {
        self.send_cmd(|reply| BridgeCommand::Cancel {
            session_id: session_id.clone(),
            reply,
        })
        .await
    }

    pub async fn set_mode(
        &self,
        mode: &str,
        session_id: Option<&SessionId>,
    ) -> crate::types::ModeResult {
        self.send_cmd(|reply| BridgeCommand::SetMode {
            mode: mode.to_string(),
            session_id: session_id.cloned(),
            reply,
        })
        .await
    }

    pub async fn set_cwd(
        &self,
        path: PathBuf,
        session_id: Option<&SessionId>,
    ) -> crate::types::CwdResult {
        self.send_cmd(|reply| BridgeCommand::SetCwd {
            path,
            session_id: session_id.cloned(),
            reply,
        })
        .await
    }

    pub async fn get_cwd_and_subdirs(&self) -> (PathBuf, Vec<PathBuf>) {
        self.send_cmd(|reply| BridgeCommand::GetCwdAndSubdirs { reply })
            .await
    }

    pub async fn prompt(
        &self,
        session_id: SessionId,
        content: Vec<ContentBlock>,
    ) -> Result<crate::types::PromptResult> {
        self.send_cmd(|reply| BridgeCommand::Prompt {
            session_id,
            content,
            reply,
        })
        .await
    }

    pub async fn respond_permission(
        &self,
        response: PermissionResponse,
        session_id: &SessionId,
    ) -> crate::types::PermissionResult {
        self.send_cmd(|reply| BridgeCommand::RespondPermission {
            response,
            session_id: session_id.clone(),
            reply,
        })
        .await
    }

    pub async fn override_pending_perm(&self, session_id: &SessionId) -> bool {
        self.send_cmd(|reply| BridgeCommand::OverridePendingPermissionRequest {
            session_id: session_id.clone(),
            reply,
        })
        .await
    }

    pub async fn respond_elicitation(
        &self,
        text: &str,
        schema: Option<&agent_client_protocol::ElicitationSchema>,
        session_id: &SessionId,
    ) -> bool {
        self.send_cmd(|reply| BridgeCommand::RespondElicitation {
            text: text.to_string(),
            schema: schema.cloned(),
            session_id: session_id.clone(),
            reply,
        })
        .await
    }

    pub async fn set_provider(&self, name: &str) -> Result<()> {
        self.send_cmd(|reply| BridgeCommand::SetProvider {
            name: name.to_string(),
            reply,
        })
        .await
    }

    pub async fn list_providers(&self) -> Option<(String, Vec<AgentInfo>)> {
        self.send_cmd(|reply| BridgeCommand::ListProviders { reply })
            .await
    }

    pub fn update_projects(&self, projects: Vec<Project>) {
        let _ = self.tx.try_send(BridgeCommand::UpdateProjects { projects });
    }

    pub async fn session_info(&self, session_id: &SessionId) -> Option<SessionInfo> {
        self.send_cmd(|reply| BridgeCommand::SessionInfo {
            session_id: session_id.clone(),
            reply,
        })
        .await
    }

    /// Send a plain-text or markdown reply directly to an IM peer. Used by the
    /// inbound handler for command responses (not part of the agent stream).
    pub async fn reply(
        &self,
        source_id: &str,
        peer: &PeerRef,
        text: &str,
        markdown: bool,
    ) -> Result<()> {
        self.router
            .reply_text(source_id, peer, text, markdown)
            .await
    }

    /// Send the `/sessions` reply, letting the IM adapter render the
    /// structured snapshot natively (falls back to `fallback_markdown`).
    pub async fn reply_session_info(
        &self,
        source_id: &str,
        peer: &PeerRef,
        info: Option<&SessionInfo>,
        fallback_markdown: &str,
    ) -> Result<()> {
        self.router
            .reply_session_info(source_id, peer, info, fallback_markdown)
            .await
    }

    /// Send the `/agent` list reply, letting the IM adapter render it
    /// natively (falls back to `fallback_markdown`).
    pub async fn reply_agent_list(
        &self,
        source_id: &str,
        peer: &PeerRef,
        current: &str,
        agents: &[AgentInfo],
        fallback_markdown: &str,
    ) -> Result<()> {
        self.router
            .reply_agent_list(source_id, peer, current, agents, fallback_markdown)
            .await
    }

    pub fn config(&self) -> &BridgeConfig {
        &self.cfg
    }

    pub fn router(&self) -> &Arc<SourceRouter> {
        &self.router
    }
}

// ── BridgeActor (internal) ─────────────────────────────────────────────

struct BridgeActor {
    rx: mpsc::Receiver<BridgeCommand>,
    tx: mpsc::Sender<BridgeCommand>,
    state: BridgeState,
    agent: Arc<dyn AgentBackend>,
    agent_factory: Option<Arc<dyn AgentFactory>>,
    router: Arc<SourceRouter>,
    cfg: BridgeConfig,
    prompt_task: Option<JoinHandle<()>>,
    handler: Arc<dyn crate::source::InboundHandler>,
}

impl BridgeActor {
    fn new(
        rx: mpsc::Receiver<BridgeCommand>,
        tx: mpsc::Sender<BridgeCommand>,
        router: Arc<SourceRouter>,
        agent: Arc<dyn AgentBackend>,
        cfg: BridgeConfig,
        handler: Arc<dyn crate::source::InboundHandler>,
    ) -> Self {
        let state = BridgeState::new(cfg.default_cwd.clone(), cfg.agent_name.clone());
        Self {
            rx,
            tx,
            state,
            agent,
            agent_factory: None,
            router,
            cfg,
            prompt_task: None,
            handler,
        }
    }

    async fn run(mut self, bridge_handle: Bridge, handler: Arc<dyn crate::source::InboundHandler>) {
        // Start SourceRouter inbound stream
        let mut inbound = match self.router.start().await {
            Ok(rx) => rx,
            Err(e) => {
                tracing::error!(error=%e, "failed to start router");
                return;
            }
        };

        loop {
            tokio::select! {
                cmd = self.rx.recv() => {
                    match cmd {
                        Some(cmd) => {
                            if self.handle_command(cmd).await {
                                break; // Shutdown
                            }
                        }
                        None => break, // All senders dropped
                    }
                }
                routed = inbound.recv() => {
                    match routed {
                        Some(msg) => {
                            let h = handler.clone();
                            let b = bridge_handle.clone();
                            tokio::spawn(async move {
                                if let Err(e) = h.handle(&b, msg).await {
                                    tracing::error!(error=%e, "inbound handler failed");
                                }
                            });
                        }
                        None => break, // All sources closed
                    }
                }
            }
        }

        self.shutdown().await;
        self.router.shutdown().await;
    }

    /// Returns true if the actor should shut down.
    async fn handle_command(&mut self, cmd: BridgeCommand) -> bool {
        match cmd {
            BridgeCommand::NewSession {
                source_id,
                peer,
                reply,
            } => {
                let result = self.cmd_new_session(source_id, peer).await;
                let _ = reply.send(result);
            }
            BridgeCommand::CloseSession { session_id, reply } => {
                let result = self.cmd_close_session(&session_id).await;
                let _ = reply.send(result);
            }
            BridgeCommand::Cancel { session_id, reply } => {
                let result = self.cmd_cancel(&session_id).await;
                let _ = reply.send(result);
            }
            BridgeCommand::SetMode {
                mode,
                session_id,
                reply,
            } => {
                let result = self.cmd_set_mode(&mode, session_id.as_ref());
                let _ = reply.send(result);
            }
            BridgeCommand::SetCwd {
                path,
                session_id,
                reply,
            } => {
                let result = self.cmd_set_cwd(path, session_id.as_ref()).await;
                let _ = reply.send(result);
            }
            BridgeCommand::GetCwdAndSubdirs { reply } => {
                let result = self.cmd_get_cwd_and_subdirs();
                let _ = reply.send(result);
            }
            BridgeCommand::Prompt {
                session_id,
                content,
                reply,
            } => {
                let result = self.cmd_prompt(session_id, content).await;
                let _ = reply.send(result);
            }
            BridgeCommand::RespondPermission {
                response,
                session_id,
                reply,
            } => {
                let result = self.cmd_respond_permission(response, &session_id);
                let _ = reply.send(result);
            }
            BridgeCommand::OverridePendingPermissionRequest { session_id, reply } => {
                let result = self.cmd_override_pending_perm(&session_id).await;
                let _ = reply.send(result);
            }
            BridgeCommand::RespondElicitation {
                text,
                schema,
                session_id,
                reply,
            } => {
                let result = self
                    .cmd_respond_elicitation(&text, schema, &session_id)
                    .await;
                let _ = reply.send(result);
            }
            BridgeCommand::SetProvider { name, reply } => {
                let result = self.cmd_set_provider(&name).await;
                let _ = reply.send(result);
            }
            BridgeCommand::ListProviders { reply } => {
                let result = self.cmd_list_providers();
                let _ = reply.send(result);
            }
            BridgeCommand::SessionInfo { session_id, reply } => {
                let result = self.cmd_session_info(&session_id);
                let _ = reply.send(result);
            }

            // Internal commands from relay/scheduler
            BridgeCommand::InsertPendingPermissionRequest { session_id, perm } => {
                self.state.pending_perms.insert(session_id, perm);
            }
            BridgeCommand::InsertPendingElicit { session_id, elicit } => {
                self.state.pending_elicits.insert(session_id, elicit);
            }
            BridgeCommand::GetPermDecision {
                session_id,
                tool_kind,
                what,
                reply,
            } => {
                let decision = self
                    .state
                    .sessions
                    .get(&session_id)
                    .map(|s| s.perm.evaluate(tool_kind, &what, &s.cwd))
                    .unwrap_or(PermissionDecision::Ask);
                let _ = reply.send(decision);
            }
            BridgeCommand::SetRunningSession { session_id } => {
                self.state.running_session = Some(session_id);
            }
            BridgeCommand::ClearRunningSession => {
                self.state.running_session = None;
            }
            BridgeCommand::RemoveSession { session_id } => {
                self.state.sessions.remove(&session_id);
            }
            BridgeCommand::PromptTaskDone => {
                self.prompt_task = None;
                self.start_next_if_idle().await;
            }
            BridgeCommand::SetAgentFactory { factory } => {
                self.agent_factory = Some(factory);
            }
            BridgeCommand::UpdateProjects { projects } => {
                self.cfg.projects = projects;
            }
        }
        false
    }

    async fn shutdown(&mut self) {
        if let Some(handle) = self.prompt_task.take() {
            let _ = handle.await;
        }
        self.state.pending_prompts.clear();
        self.state.running_session = None;
        let asids: Vec<_> = self
            .state
            .sessions
            .drain()
            .map(|(_, s)| s.agent_session_id)
            .collect();
        for asid in asids {
            let _ = self.agent.close_session(asid).await;
        }
    }

    fn publish_registry(&self) {
        if let Some(ref registry) = self.cfg.registry {
            crate::scheduler::publish_registry_from_sessions(registry, &self.state.sessions);
        }
    }

    // ── Command implementations ────────────────────────────────────────

    /// Explicitly create a new session in the bridge's current cwd, bound to
    /// `(source_id, peer)`. Replaces the old implicit `ensure_session`.
    async fn cmd_new_session(&mut self, source_id: String, peer: PeerRef) -> Result<SessionId> {
        let agent_name = self.state.agent_name.clone();
        let target = self.state.cwd.clone();

        // Handle session_base_dir: if the cwd is under the configured base,
        // mint a fresh per-session directory.
        let target_cwd = if let Some(ref base) = self.cfg.session_base_dir {
            if target == *base || target.starts_with(base) {
                let session_dir = base.join(format!("sess-{}", crate::session::format_timestamp()));
                if let Err(e) = std::fs::create_dir_all(&session_dir) {
                    tracing::error!(error=%e, dir=%session_dir.display(), "failed to create session dir");
                }
                self.state.cwd = session_dir.clone();
                crate::session::cleanup_old_sessions(base, 2);
                session_dir
            } else {
                target
            }
        } else {
            target
        };

        tracing::debug!(cwd=%target_cwd.display(), "creating agent session");
        let agent_session_id = self.agent.new_session(&target_cwd).await?;
        let short_id = self.state.sessions.next_short_id();
        let id = gen_session_id();
        tracing::debug!(session=%id, agent_session=%agent_session_id, id=short_id, "agent session ready");

        let mut perm = PermissionPolicy::new();
        if self.state.pending_yolo {
            perm.set_yolo(true);
        }
        let session = Session {
            id: id.clone(),
            bindings: vec![ChannelBinding { source_id, peer }],
            cwd: target_cwd,
            agent_session_id,
            short_id,
            agent_name,
            created_at: SystemTime::now(),
            last_active: Instant::now(),
            perm,
            project_context_sent: false,
        };
        self.state.sessions.insert(id.clone(), session);
        self.publish_registry();

        Ok(id)
    }

    async fn cmd_close_session(&mut self, session_id: &SessionId) -> Result<Option<String>> {
        let is_running = self.state.running_session.as_ref() == Some(session_id);
        let asid = self
            .state
            .sessions
            .get(session_id)
            .map(|s| s.agent_session_id.clone());

        if is_running && asid.is_some() {
            self.stop_current_prompt().await;
        }

        let active = self.state.sessions.remove(session_id);
        self.state
            .pending_prompts
            .retain(|(id, _)| id != session_id);
        self.state.pending_perms.remove(session_id);
        self.state.pending_elicits.remove(session_id);
        if self.state.running_session.as_ref() == Some(session_id) {
            self.state.running_session = None;
        }
        self.publish_registry();

        let tag = active.as_ref().map(|s| s.tag());
        if let Some(s) = active {
            let _ = self.agent.close_session(s.agent_session_id).await;
        }
        Ok(tag)
    }

    async fn cmd_cancel(&mut self, session_id: &SessionId) -> Result<Option<String>> {
        let is_running = self.state.running_session.as_ref() == Some(session_id);
        let tag = self.state.sessions.get(session_id).map(|s| s.tag());
        self.state
            .pending_prompts
            .retain(|(id, _)| id != session_id);

        if is_running && tag.is_some() {
            self.stop_current_prompt().await;
        }
        Ok(tag)
    }

    fn cmd_set_mode(
        &mut self,
        mode: &str,
        session_id: Option<&SessionId>,
    ) -> crate::types::ModeResult {
        use crate::types::ModeResult;
        match mode {
            "yolo" => {
                self.state.pending_yolo = true;
                if let Some(s) = session_id.and_then(|id| self.state.sessions.get_mut(id)) {
                    s.perm.set_yolo(true);
                    ModeResult::Applied { tag: s.tag() }
                } else {
                    ModeResult::Pending
                }
            }
            "safe" => {
                self.state.pending_yolo = false;
                if let Some(s) = session_id.and_then(|id| self.state.sessions.get_mut(id)) {
                    s.perm.set_yolo(false);
                    s.perm.clear_grants();
                    ModeResult::Applied { tag: s.tag() }
                } else {
                    ModeResult::NoSession
                }
            }
            _ => ModeResult::NoSession,
        }
    }

    async fn cmd_set_cwd(
        &mut self,
        path: PathBuf,
        session_id: Option<&SessionId>,
    ) -> crate::types::CwdResult {
        use crate::types::CwdResult;
        if !path.exists() {
            return CwdResult::NotFound;
        }
        if !path.is_dir() {
            return CwdResult::NotADir;
        }

        self.state.cwd = path.clone();
        let to_close = session_id.and_then(|id| {
            self.state.sessions.get(id).and_then(|active| {
                if active.cwd != path {
                    Some((
                        id.clone(),
                        active.agent_session_id.clone(),
                        self.state.running_session.as_ref() == Some(id),
                    ))
                } else {
                    None
                }
            })
        });

        if let Some((id, asid, is_running)) = to_close {
            if is_running {
                self.stop_current_prompt().await;
            }
            let _ = self.agent.close_session(asid).await;
            let tag = self
                .state
                .sessions
                .remove(&id)
                .map(|s| s.tag())
                .unwrap_or_default();
            self.publish_registry();
            return CwdResult::SessionClosed { tag };
        }
        CwdResult::Changed
    }

    fn cmd_get_cwd_and_subdirs(&self) -> (PathBuf, Vec<PathBuf>) {
        let cwd = self.state.cwd.clone();
        let mut dirs: Vec<PathBuf> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&cwd) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    dirs.push(p);
                }
            }
        }
        dirs.sort();
        (cwd, dirs)
    }

    async fn cmd_prompt(
        &mut self,
        session_id: SessionId,
        content: Vec<ContentBlock>,
    ) -> Result<crate::types::PromptResult> {
        use crate::types::PromptResult;
        // Reject up front rather than queueing: a session can vanish out from
        // under a caller's cached SessionId (e.g. `/agent` drains every
        // session on backend switch). Queueing it anyway means it silently
        // dies later in `start_next_if_idle` with nothing telling the caller
        // to stop reusing that id — surfacing it here lets the IM handler
        // forget the stale mapping and create a fresh session instead.
        if self.state.sessions.get(&session_id).is_none() {
            return Err(crate::error::Error::SessionNotFound(session_id.to_string()));
        }
        let was_empty = self.state.pending_prompts.is_empty();
        let queued_tag = if !was_empty {
            self.state.sessions.get(&session_id).map(|s| s.tag())
        } else {
            None
        };
        self.state.pending_prompts.push_back((session_id, content));

        self.start_next_if_idle().await;

        match queued_tag {
            Some(tag) => Ok(PromptResult::Queued { tag }),
            None => Ok(PromptResult::Started),
        }
    }

    fn cmd_respond_permission(
        &mut self,
        response: PermissionResponse,
        session_id: &SessionId,
    ) -> crate::types::PermissionResult {
        use crate::types::PermissionResult;
        let Some(pp) = self.state.pending_perms.remove(session_id) else {
            return PermissionResult::NoPending;
        };
        let effective = match self.state.sessions.get_mut(session_id) {
            Some(s) => s.perm.resolve_and_apply(pp.tool_kind, response, &pp.what),
            None => response,
        };
        let _ = pp.responder.send(effective);
        PermissionResult::Answered { effective }
    }

    async fn cmd_override_pending_perm(&mut self, session_id: &SessionId) -> bool {
        let pp = self.state.pending_perms.remove(session_id);
        if let Some(pp) = pp {
            let is_running = self.state.running_session.as_ref() == Some(session_id);
            let _ = pp.responder.send(PermissionResponse::Deny);
            if is_running {
                self.stop_current_prompt().await;
            }
            true
        } else {
            false
        }
    }

    async fn cmd_respond_elicitation(
        &mut self,
        text: &str,
        schema: Option<agent_client_protocol::ElicitationSchema>,
        session_id: &SessionId,
    ) -> bool {
        let elicit = self.state.pending_elicits.remove(session_id);
        if let Some(pe) = elicit {
            let effective_schema = pe.schema.as_ref().or(schema.as_ref());
            let response = crate::types::parse_elicit_response(text, effective_schema);
            let _ = self
                .agent
                .respond_elicitation(&pe.elicit_id, response)
                .await;
            true
        } else {
            false
        }
    }

    async fn cmd_set_provider(&mut self, name: &str) -> Result<()> {
        let factory = self
            .agent_factory
            .clone()
            .ok_or_else(|| crate::error::Error::Other("no agent factory configured".into()))?;
        let available = factory.available();
        if !available.contains(&name.to_string()) {
            return Err(crate::error::Error::Other(format!(
                "agent '{}' not found, available: {}",
                name,
                available.join(", ")
            )));
        }

        let was_running = self.state.running_session.is_some();
        self.state.pending_prompts.clear();
        self.state.pending_perms.clear();
        self.state.pending_elicits.clear();
        self.state.running_session = None;
        let asids: Vec<_> = self
            .state
            .sessions
            .drain()
            .map(|(_, s)| s.agent_session_id)
            .collect();
        // Every session we just dropped may be cached by the inbound handler
        // (e.g. the IM layer's peer → SessionId map) — tell it to forget all
        // of them now, instead of leaving it to find out the hard way (and
        // recover) on the next message for each peer.
        self.handler.invalidate_all_sessions().await;

        if was_running {
            self.stop_current_prompt().await;
        }
        for asid in asids {
            let _ = self.agent.close_session(asid).await;
        }
        self.agent.shutdown().await;

        match factory.build(name).await {
            Ok(new_agent) => {
                self.agent = new_agent;
                self.state.agent_name = name.to_string();
                self.publish_registry();
                if let Some(ref path) = self.cfg.config_path
                    && let Err(e) = persist_agent_backend(path, name)
                {
                    tracing::warn!(error=%e, "failed to persist agent.backend to config");
                }
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn cmd_list_providers(&self) -> Option<(String, Vec<AgentInfo>)> {
        let factory = self.agent_factory.as_ref()?;
        let current = self.state.agent_name.clone();
        let agents = factory.available_with_status(&current);
        Some((current, agents))
    }

    fn cmd_session_info(&self, session_id: &SessionId) -> Option<SessionInfo> {
        self.state.sessions.get(session_id).map(|s| SessionInfo {
            short_id: s.short_id,
            session_id: s.id.to_string(),
            agent_name: s.agent_name.clone(),
            cwd: s.cwd.clone(),
            created_at: s.created_at,
            idle_duration: s.last_active.elapsed(),
            is_yolo: s.perm.is_yolo(),
            grant_summary: s.perm.grant_summary(),
        })
    }

    // ── Prompt lifecycle ───────────────────────────────────────────────

    async fn stop_current_prompt(&mut self) {
        if let Some(asid) = self
            .state
            .running_session
            .as_ref()
            .and_then(|id| self.state.sessions.get(id))
            .map(|s| s.agent_session_id.clone())
        {
            let _ = self.agent.cancel(&asid).await;
        }
        if let Some(handle) = self.prompt_task.take() {
            handle.abort();
            let _ = handle.await;
        }
    }

    async fn start_next_if_idle(&mut self) {
        // Check if a prompt task is already running
        if let Some(ref handle) = self.prompt_task
            && !handle.is_finished()
        {
            return;
        }
        self.prompt_task = None;

        let Some((session_id, content)) = self.state.pending_prompts.pop_front() else {
            return;
        };

        // The session may have been closed while queued.
        if self.state.sessions.get(&session_id).is_none() {
            tracing::debug!(session=%session_id, "queued prompt for closed session; dropping");
            return;
        }

        // Refresh the agent subprocess session if it idle-expired, keeping the
        // external SessionId stable.
        if let Err(e) = self.refresh_agent_if_idle(&session_id).await {
            tracing::error!(error=%e, "agent session refresh failed");
            self.send_error_to_bindings(&session_id, e.to_string())
                .await;
            return;
        }

        let Some(session) = self.state.sessions.get_mut(&session_id) else {
            return;
        };
        session.last_active = Instant::now();
        let agent_session_id = session.agent_session_id.clone();
        let tag = session.tag();
        let bindings = session.bindings.clone();
        let target_cwd = session.cwd.clone();

        // Inject project context if needed
        let content = self.maybe_inject_project_context(&session_id, content);

        // Spawn relay task
        let tx = self.tx.clone();
        let router = self.router.clone();
        let agent = self.agent.clone();
        let cfg = self.cfg.clone();
        let sid_t = session_id.clone();
        let bindings_t = bindings.clone();
        let tag_err = tag.clone();

        let handle = tokio::spawn(async move {
            let _ = tx
                .send(BridgeCommand::SetRunningSession {
                    session_id: sid_t.clone(),
                })
                .await;

            if let Err(e) = crate::relay::run_prompt_task_routed(
                router.clone(),
                agent,
                tx.clone(),
                cfg,
                sid_t.clone(),
                bindings_t.clone(),
                agent_session_id,
                tag,
                target_cwd,
                content,
            )
            .await
            {
                tracing::error!(error=%e, "prompt task failed");
                let _ = tx
                    .send(BridgeCommand::RemoveSession {
                        session_id: sid_t.clone(),
                    })
                    .await;
                let ev = AgentEvent::new(
                    sid_t.clone(),
                    tag_err.clone(),
                    AgentUpdate::Error(e.to_string()),
                );
                for b in &bindings_t {
                    let _ = router.send_update(&b.source_id, &b.peer, &ev).await;
                }
            }

            let _ = tx.send(BridgeCommand::ClearRunningSession).await;
            let _ = tx.send(BridgeCommand::PromptTaskDone).await;
        });

        self.prompt_task = Some(handle);
    }

    /// If the session's agent subprocess has idle-expired, close it and create
    /// a fresh one under the same external [`SessionId`].
    async fn refresh_agent_if_idle(&mut self, session_id: &SessionId) -> Result<()> {
        let idle_timeout = self.cfg.session_idle_timeout;
        if idle_timeout.is_zero() {
            return Ok(());
        }
        let stale = self
            .state
            .sessions
            .get(session_id)
            .map(|s| s.last_active.elapsed() > idle_timeout)
            .unwrap_or(false);
        if !stale {
            return Ok(());
        }

        let Some((old_asid, cwd)) = self
            .state
            .sessions
            .get(session_id)
            .map(|s| (s.agent_session_id.clone(), s.cwd.clone()))
        else {
            return Ok(());
        };
        tracing::debug!(session=%session_id, "session idle-expired; recreating agent subprocess");
        let _ = self.agent.close_session(old_asid).await;
        let new_asid = self.agent.new_session(&cwd).await?;
        if let Some(s) = self.state.sessions.get_mut(session_id) {
            s.agent_session_id = new_asid;
            s.project_context_sent = false;
        }
        Ok(())
    }

    async fn send_error_to_bindings(&self, session_id: &SessionId, msg: String) {
        if let Some(s) = self.state.sessions.get(session_id) {
            let ev = AgentEvent::new(session_id.clone(), s.tag(), AgentUpdate::Error(msg));
            for b in &s.bindings {
                let _ = self.router.send_update(&b.source_id, &b.peer, &ev).await;
            }
        }
    }

    fn maybe_inject_project_context(
        &mut self,
        session_id: &SessionId,
        mut content: Vec<ContentBlock>,
    ) -> Vec<ContentBlock> {
        use crate::types::TextContent;

        let should_inject = self
            .state
            .sessions
            .get(session_id)
            .map(|s| !s.project_context_sent)
            .unwrap_or(false);

        if !should_inject {
            return content;
        }

        if let Some(s) = self.state.sessions.get_mut(session_id) {
            s.project_context_sent = true;
        }

        if self.cfg.projects.is_empty() {
            return content;
        }

        let cwd = self.state.cwd.display().to_string();
        let mut ctx = String::new();
        ctx.push_str("Available projects:\n");
        for (i, p) in self.cfg.projects.iter().enumerate() {
            ctx.push_str(&format!("{}. {} — {}\n", i + 1, p.name, p.git_url));
        }
        ctx.push_str(&format!(
            "\nWorking directory: {}\n\n\
             The git repositories above are available if needed. \
             Clone them on demand when the user asks to work on a project. \
             Do not clone unless requested.\n\
             ---",
            cwd,
        ));
        content.insert(0, ContentBlock::Text(TextContent::new(ctx)));
        content
    }
}

/// Update `agent.backend = "<value>"` in a TOML config file, preserving
/// all other content. Pure string manipulation — no TOML parser needed.
fn persist_agent_backend(path: &std::path::Path, value: &str) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let mut result = Vec::new();
    let mut in_section = false;
    let mut done = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = !done && trimmed == "[agent]";
        }
        if in_section && !done && trimmed.starts_with("backend") {
            result.push(format!("backend = \"{}\"", value));
            done = true;
            in_section = false;
        } else {
            result.push(line.to_string());
        }
    }

    if !done {
        if !result.iter().any(|l| l.trim() == "[agent]") {
            result.push(String::new());
            result.push("[agent]".to_string());
        }
        result.push(format!("backend = \"{}\"", value));
    }

    std::fs::write(path, result.join("\n"))
}

#[cfg(test)]
#[path = "bridge_tests.rs"]
mod tests;
