use crate::agent::{AgentBackend, AgentFactory, AgentInfo};
use crate::error::Result;
use crate::event::OutboundEvent;
use crate::permission::{PendingPerm, PermResponse, PermissionDecision, PermissionPolicy};
use crate::registry::SessionRegistry;
use crate::router::SourceRouter;
use crate::session::{ManagedSession, SessionKey};
use crate::state::{BridgeState, PendingElicit};
use crate::types::{ElicitField, PeerRef, Project, SessionId, SessionInfoSnapshot, ToolKind};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
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
    CloseSession {
        key: SessionKey,
        reply: oneshot::Sender<Result<Option<String>>>,
    },
    CancelPrompt {
        key: SessionKey,
        reply: oneshot::Sender<Result<Option<String>>>,
    },
    SetYolo {
        on: bool,
        key: SessionKey,
        reply: oneshot::Sender<crate::types::YoloResult>,
    },
    SetSafe {
        key: SessionKey,
        reply: oneshot::Sender<crate::types::SafeResult>,
    },
    SetCwd {
        path: PathBuf,
        key: SessionKey,
        reply: oneshot::Sender<crate::types::CwdResult>,
    },
    GetCwdAndSubdirs {
        reply: oneshot::Sender<(PathBuf, Vec<PathBuf>)>,
    },
    SendPrompt {
        key: SessionKey,
        peer: PeerRef,
        text: String,
        source_id: String,
        reply: oneshot::Sender<Result<crate::types::PromptResult>>,
    },
    AnswerPermission {
        response: PermResponse,
        key: SessionKey,
        reply: oneshot::Sender<crate::types::PermAnswerResult>,
    },
    OverridePendingPerm {
        key: SessionKey,
        reply: oneshot::Sender<bool>,
    },
    AnswerElicitation {
        text: String,
        schema: Option<Vec<ElicitField>>,
        key: SessionKey,
        reply: oneshot::Sender<bool>,
    },
    SwitchAgent {
        name: String,
        reply: oneshot::Sender<Result<()>>,
    },
    ListAgents {
        reply: oneshot::Sender<Option<(String, Vec<AgentInfo>)>>,
    },
    SessionInfo {
        key: SessionKey,
        reply: oneshot::Sender<Option<SessionInfoSnapshot>>,
    },

    // Internal (used by relay/scheduler spawn tasks)
    InsertPendingPerm {
        key: SessionKey,
        perm: PendingPerm,
    },
    InsertPendingElicit {
        key: SessionKey,
        elicit: PendingElicit,
    },
    GetPermDecision {
        key: SessionKey,
        tool_kind: ToolKind,
        what: String,
        reply: oneshot::Sender<PermissionDecision>,
    },
    GetSessionCwd {
        key: SessionKey,
        reply: oneshot::Sender<PathBuf>,
    },
    SetRunningSession {
        key: SessionKey,
    },
    ClearRunningSession,
    RemoveSession {
        key: SessionKey,
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

        let actor = BridgeActor::new(rx, tx.clone(), router.clone(), agent, (*cfg).clone());
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

    pub async fn close_session(&self, key: &SessionKey) -> Result<Option<String>> {
        self.send_cmd(|reply| BridgeCommand::CloseSession {
            key: key.clone(),
            reply,
        })
        .await
    }

    pub async fn cancel_prompt(&self, key: &SessionKey) -> Result<Option<String>> {
        self.send_cmd(|reply| BridgeCommand::CancelPrompt {
            key: key.clone(),
            reply,
        })
        .await
    }

    pub async fn set_yolo(&self, on: bool, key: &SessionKey) -> crate::types::YoloResult {
        self.send_cmd(|reply| BridgeCommand::SetYolo {
            on,
            key: key.clone(),
            reply,
        })
        .await
    }

    pub async fn set_safe(&self, key: &SessionKey) -> crate::types::SafeResult {
        self.send_cmd(|reply| BridgeCommand::SetSafe {
            key: key.clone(),
            reply,
        })
        .await
    }

    pub async fn set_cwd(&self, path: PathBuf, key: &SessionKey) -> crate::types::CwdResult {
        self.send_cmd(|reply| BridgeCommand::SetCwd {
            path,
            key: key.clone(),
            reply,
        })
        .await
    }

    pub async fn get_cwd_and_subdirs(&self) -> (PathBuf, Vec<PathBuf>) {
        self.send_cmd(|reply| BridgeCommand::GetCwdAndSubdirs { reply })
            .await
    }

    pub async fn send_prompt(
        &self,
        key: SessionKey,
        peer: PeerRef,
        text: String,
        source_id: String,
    ) -> Result<crate::types::PromptResult> {
        self.send_cmd(|reply| BridgeCommand::SendPrompt {
            key,
            peer,
            text,
            source_id,
            reply,
        })
        .await
    }

    pub async fn answer_permission(
        &self,
        response: PermResponse,
        key: &SessionKey,
    ) -> crate::types::PermAnswerResult {
        self.send_cmd(|reply| BridgeCommand::AnswerPermission {
            response,
            key: key.clone(),
            reply,
        })
        .await
    }

    pub async fn override_pending_perm(&self, key: &SessionKey) -> bool {
        self.send_cmd(|reply| BridgeCommand::OverridePendingPerm {
            key: key.clone(),
            reply,
        })
        .await
    }

    pub async fn answer_elicitation(
        &self,
        text: &str,
        schema: Option<&[ElicitField]>,
        key: &SessionKey,
    ) -> bool {
        self.send_cmd(|reply| BridgeCommand::AnswerElicitation {
            text: text.to_string(),
            schema: schema.map(|s| s.to_vec()),
            key: key.clone(),
            reply,
        })
        .await
    }

    pub async fn switch_agent(&self, name: &str) -> Result<()> {
        self.send_cmd(|reply| BridgeCommand::SwitchAgent {
            name: name.to_string(),
            reply,
        })
        .await
    }

    pub async fn list_agents(&self) -> Option<(String, Vec<AgentInfo>)> {
        self.send_cmd(|reply| BridgeCommand::ListAgents { reply })
            .await
    }

    pub fn update_projects(&self, projects: Vec<Project>) {
        let _ = self.tx.try_send(BridgeCommand::UpdateProjects { projects });
    }

    pub async fn session_info(&self, key: &SessionKey) -> Option<SessionInfoSnapshot> {
        self.send_cmd(|reply| BridgeCommand::SessionInfo {
            key: key.clone(),
            reply,
        })
        .await
    }

    pub async fn send_event(
        &self,
        source_id: &str,
        peer: &PeerRef,
        event: &OutboundEvent,
    ) -> Result<()> {
        self.router.send_event(source_id, peer, event).await
    }

    pub fn config(&self) -> &BridgeConfig {
        &self.cfg
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
}

impl BridgeActor {
    fn new(
        rx: mpsc::Receiver<BridgeCommand>,
        tx: mpsc::Sender<BridgeCommand>,
        router: Arc<SourceRouter>,
        agent: Arc<dyn AgentBackend>,
        cfg: BridgeConfig,
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
            BridgeCommand::CloseSession { key, reply } => {
                let result = self.cmd_close_session(&key).await;
                let _ = reply.send(result);
            }
            BridgeCommand::CancelPrompt { key, reply } => {
                let result = self.cmd_cancel_prompt(&key).await;
                let _ = reply.send(result);
            }
            BridgeCommand::SetYolo { on, key, reply } => {
                let result = self.cmd_set_yolo(on, &key);
                let _ = reply.send(result);
            }
            BridgeCommand::SetSafe { key, reply } => {
                let result = self.cmd_set_safe(&key);
                let _ = reply.send(result);
            }
            BridgeCommand::SetCwd { path, key, reply } => {
                let result = self.cmd_set_cwd(path, &key).await;
                let _ = reply.send(result);
            }
            BridgeCommand::GetCwdAndSubdirs { reply } => {
                let result = self.cmd_get_cwd_and_subdirs();
                let _ = reply.send(result);
            }
            BridgeCommand::SendPrompt {
                key,
                peer,
                text,
                source_id,
                reply,
            } => {
                let result = self.cmd_send_prompt(key, peer, text, source_id).await;
                let _ = reply.send(result);
            }
            BridgeCommand::AnswerPermission {
                response,
                key,
                reply,
            } => {
                let result = self.cmd_answer_permission(response, &key);
                let _ = reply.send(result);
            }
            BridgeCommand::OverridePendingPerm { key, reply } => {
                let result = self.cmd_override_pending_perm(&key).await;
                let _ = reply.send(result);
            }
            BridgeCommand::AnswerElicitation {
                text,
                schema,
                key,
                reply,
            } => {
                let result = self.cmd_answer_elicitation(&text, schema, &key).await;
                let _ = reply.send(result);
            }
            BridgeCommand::SwitchAgent { name, reply } => {
                let result = self.cmd_switch_agent(&name).await;
                let _ = reply.send(result);
            }
            BridgeCommand::ListAgents { reply } => {
                let result = self.cmd_list_agents();
                let _ = reply.send(result);
            }
            BridgeCommand::SessionInfo { key, reply } => {
                let result = self.cmd_session_info(&key);
                let _ = reply.send(result);
            }

            // Internal commands from relay/scheduler
            BridgeCommand::InsertPendingPerm { key, perm } => {
                self.state.pending_perms.insert(key, perm);
            }
            BridgeCommand::InsertPendingElicit { key, elicit } => {
                self.state.pending_elicits.insert(key, elicit);
            }
            BridgeCommand::GetPermDecision {
                key,
                tool_kind,
                what,
                reply,
            } => {
                let decision = self
                    .state
                    .sessions
                    .get(&key)
                    .map(|a| a.perm.evaluate(tool_kind, &what))
                    .unwrap_or(PermissionDecision::Ask);
                let _ = reply.send(decision);
            }
            BridgeCommand::GetSessionCwd { key, reply } => {
                let cwd = self
                    .state
                    .sessions
                    .get(&key)
                    .map(|a| a.cwd.clone())
                    .unwrap_or_else(|| self.cfg.default_cwd.clone());
                let _ = reply.send(cwd);
            }
            BridgeCommand::SetRunningSession { key } => {
                self.state.running_session = Some(key);
            }
            BridgeCommand::ClearRunningSession => {
                self.state.running_session = None;
            }
            BridgeCommand::RemoveSession { key } => {
                self.state.sessions.remove(&key);
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
        let sids: Vec<SessionId> = self
            .state
            .sessions
            .drain()
            .map(|(_, ms)| ms.session_id)
            .collect();
        for sid in sids {
            let _ = self.agent.close_session(sid).await;
        }
    }

    fn publish_registry(&self) {
        if let Some(ref registry) = self.cfg.registry {
            crate::scheduler::publish_registry_from_sessions(registry, &self.state.sessions);
        }
    }

    // ── Command implementations ────────────────────────────────────────

    async fn cmd_close_session(&mut self, key: &SessionKey) -> Result<Option<String>> {
        let is_running = self.state.running_session.as_ref() == Some(key);
        let sid = self.state.sessions.get(key).map(|a| a.session_id.clone());

        if is_running && let Some(ref sid) = sid {
            self.stop_current_prompt(sid).await;
        }

        let active = self.state.sessions.remove(key);
        self.state.pending_prompts.retain(|(k, _, _, _)| k != key);
        self.state.pending_perms.remove(key);
        self.state.pending_elicits.remove(key);
        if self.state.running_session.as_ref() == Some(key) {
            self.state.running_session = None;
        }
        self.publish_registry();

        let tag = active.as_ref().map(|a| a.tag());
        if let Some(a) = active {
            let _ = self.agent.close_session(a.session_id).await;
        }
        Ok(tag)
    }

    async fn cmd_cancel_prompt(&mut self, key: &SessionKey) -> Result<Option<String>> {
        let is_running = self.state.running_session.as_ref() == Some(key);
        let info = self
            .state
            .sessions
            .get(key)
            .map(|a| (a.session_id.clone(), a.tag()));
        self.state.pending_prompts.retain(|(k, _, _, _)| k != key);

        if is_running && let Some((ref sid, _)) = info {
            self.stop_current_prompt(sid).await;
        }
        Ok(info.map(|(_, tag)| tag))
    }

    fn cmd_set_yolo(&mut self, on: bool, key: &SessionKey) -> crate::types::YoloResult {
        use crate::types::YoloResult;
        self.state.pending_yolo = on;
        if let Some(a) = self.state.sessions.get_mut(key) {
            a.perm.set_yolo(on);
            YoloResult::Applied { tag: a.tag() }
        } else {
            YoloResult::Pending
        }
    }

    fn cmd_set_safe(&mut self, key: &SessionKey) -> crate::types::SafeResult {
        use crate::types::SafeResult;
        self.state.pending_yolo = false;
        if let Some(a) = self.state.sessions.get_mut(key) {
            a.perm.set_yolo(false);
            a.perm.clear_grants();
            SafeResult::Applied { tag: a.tag() }
        } else {
            SafeResult::NoSession
        }
    }

    async fn cmd_set_cwd(&mut self, path: PathBuf, key: &SessionKey) -> crate::types::CwdResult {
        use crate::types::CwdResult;
        if !path.exists() {
            return CwdResult::NotFound;
        }
        if !path.is_dir() {
            return CwdResult::NotADir;
        }

        self.state.cwd = path.clone();
        let to_close = self.state.sessions.get(key).and_then(|active| {
            if active.cwd != path {
                Some((
                    active.session_id.clone(),
                    self.state.running_session.as_ref() == Some(key),
                ))
            } else {
                None
            }
        });

        if let Some((sid, is_running)) = to_close {
            if is_running {
                self.stop_current_prompt(&sid).await;
            }
            let _ = self.agent.close_session(sid).await;
            let tag = self
                .state
                .sessions
                .remove(key)
                .map(|a| a.tag())
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

    async fn cmd_send_prompt(
        &mut self,
        key: SessionKey,
        peer: PeerRef,
        text: String,
        source_id: String,
    ) -> Result<crate::types::PromptResult> {
        use crate::types::PromptResult;
        let was_empty = self.state.pending_prompts.is_empty();
        self.state
            .pending_prompts
            .push_back((key.clone(), peer, text, source_id));
        let queued_tag = if !was_empty {
            self.state.sessions.get(&key).map(|a| a.tag())
        } else {
            None
        };

        self.start_next_if_idle().await;

        match queued_tag {
            Some(tag) => Ok(PromptResult::Queued { tag }),
            None => Ok(PromptResult::Started),
        }
    }

    fn cmd_answer_permission(
        &mut self,
        response: PermResponse,
        key: &SessionKey,
    ) -> crate::types::PermAnswerResult {
        use crate::types::PermAnswerResult;
        let Some(pp) = self.state.pending_perms.remove(key) else {
            return PermAnswerResult::NoPending;
        };
        let effective = match self.state.sessions.get_mut(key) {
            Some(a) => a.perm.resolve_and_apply(pp.tool_kind, response, &pp.what),
            None => response,
        };
        let _ = pp.responder.send(effective);
        PermAnswerResult::Answered { effective }
    }

    async fn cmd_override_pending_perm(&mut self, key: &SessionKey) -> bool {
        let pp = self.state.pending_perms.remove(key);
        if let Some(pp) = pp {
            let is_running = self.state.running_session.as_ref() == Some(key);
            let _ = pp.responder.send(PermResponse::Deny);
            if is_running {
                self.stop_current_prompt(&pp.session_id).await;
            }
            true
        } else {
            false
        }
    }

    async fn cmd_answer_elicitation(
        &mut self,
        text: &str,
        schema: Option<Vec<ElicitField>>,
        key: &SessionKey,
    ) -> bool {
        let elicit = self.state.pending_elicits.remove(key);
        if let Some(pe) = elicit {
            let response = crate::types::parse_elicit_response(
                text,
                pe.schema.as_deref().or(schema.as_deref()),
            );
            let _ = self.agent.answer_elicitation(&pe.elicit_id, response).await;
            true
        } else {
            false
        }
    }

    async fn cmd_switch_agent(&mut self, name: &str) -> Result<()> {
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

        let running_sid = self
            .state
            .running_session
            .as_ref()
            .and_then(|key| self.state.sessions.get(key).map(|a| a.session_id.clone()));
        self.state.pending_prompts.clear();
        self.state.pending_perms.clear();
        self.state.pending_elicits.clear();
        self.state.running_session = None;
        let sids: Vec<SessionId> = self
            .state
            .sessions
            .drain()
            .map(|(_, ms)| ms.session_id)
            .collect();

        if let Some(ref sid) = running_sid {
            self.stop_current_prompt(sid).await;
        }
        for sid in sids {
            let _ = self.agent.close_session(sid).await;
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

    fn cmd_list_agents(&self) -> Option<(String, Vec<AgentInfo>)> {
        let factory = self.agent_factory.as_ref()?;
        let current = self.state.agent_name.clone();
        let agents = factory.available_with_status(&current);
        Some((current, agents))
    }

    fn cmd_session_info(&self, key: &SessionKey) -> Option<SessionInfoSnapshot> {
        self.state.sessions.get(key).map(|a| SessionInfoSnapshot {
            short_id: a.short_id,
            session_id: a.session_id.as_str().to_string(),
            agent_name: a.agent_name.clone(),
            cwd: a.cwd.clone(),
            created_at: a.created_at,
            idle_duration: a.last_active.elapsed(),
            is_yolo: a.perm.is_yolo(),
            grant_summary: a.perm.grant_summary(),
        })
    }

    // ── Prompt lifecycle ───────────────────────────────────────────────

    async fn stop_current_prompt(&mut self, sid: &SessionId) {
        let _ = self.agent.cancel(sid).await;
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

        let Some((key, peer, text, source_id)) = self.state.pending_prompts.pop_front() else {
            return;
        };

        // Ensure session
        let session_result = self.ensure_session(&key, &peer).await;
        let (sid, tag) = match session_result {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(error=%e, "ensure_session failed");
                let _ = self
                    .router
                    .send_event(&key.source_id, &peer, &OutboundEvent::Error(e.to_string()))
                    .await;
                return;
            }
        };

        // Inject project context if needed
        let text = self.maybe_inject_project_context(&key, text);

        // Spawn relay task
        let tx = self.tx.clone();
        let router = self.router.clone();
        let agent = self.agent.clone();
        let cfg = self.cfg.clone();
        let key_t = key.clone();
        let peer_t = peer.clone();
        let sid_t = sid.clone();

        let handle = tokio::spawn(async move {
            let _ = tx
                .send(BridgeCommand::SetRunningSession { key: key_t.clone() })
                .await;

            if let Err(e) = crate::relay::run_prompt_task_routed(
                router.clone(),
                source_id,
                agent,
                tx.clone(),
                cfg,
                key_t.clone(),
                peer_t.clone(),
                sid_t,
                tag,
                text,
            )
            .await
            {
                tracing::error!(error=%e, "prompt task failed");
                let _ = tx
                    .send(BridgeCommand::RemoveSession { key: key_t.clone() })
                    .await;
                let _ = router
                    .send_event(
                        &key_t.source_id,
                        &peer_t,
                        &OutboundEvent::Error(e.to_string()),
                    )
                    .await;
            }

            let _ = tx.send(BridgeCommand::ClearRunningSession).await;
            let _ = tx.send(BridgeCommand::PromptTaskDone).await;
        });

        self.prompt_task = Some(handle);
    }

    fn maybe_inject_project_context(&mut self, key: &SessionKey, text: String) -> String {
        let should_inject = self
            .state
            .sessions
            .get(key)
            .map(|m| !m.project_context_sent)
            .unwrap_or(false);

        if !should_inject {
            return text;
        }

        if let Some(m) = self.state.sessions.get_mut(key) {
            m.project_context_sent = true;
        }

        if self.cfg.projects.is_empty() {
            return text;
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
             ---\n\n{}",
            cwd, text
        ));
        ctx
    }

    async fn ensure_session(
        &mut self,
        key: &SessionKey,
        peer: &PeerRef,
    ) -> Result<(SessionId, String)> {
        let idle_timeout = self.cfg.session_idle_timeout;
        let agent_name = self.state.agent_name.clone();
        let target = self.state.cwd.clone();

        // Try to reuse existing session
        let reuse = self.state.sessions.get(key).and_then(|a| {
            if a.cwd != target {
                return None;
            }
            if !idle_timeout.is_zero() && a.last_active.elapsed() > idle_timeout {
                tracing::debug!(
                    session = %a.session_id,
                    idle_secs = a.last_active.elapsed().as_secs(),
                    "session idle-expired; will recreate"
                );
                return None;
            }
            Some((a.session_id.clone(), a.tag()))
        });

        if let Some((sid, tag)) = reuse {
            if let Some(a) = self.state.sessions.get_mut(key) {
                a.last_active = Instant::now();
            }
            return Ok((sid, tag));
        }

        // Close old session if any
        let to_close = self.state.sessions.remove(key);
        if let Some(a) = to_close {
            let _ = self.agent.close_session(a.session_id).await;
        }

        // Handle session_base_dir
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
        let sid = self.agent.new_session(&target_cwd).await?;
        let short_id = self.state.sessions.next_short_id();
        tracing::debug!(session=%sid.as_str(), id=short_id, "agent session ready");

        let mut perm = PermissionPolicy::new();
        if self.state.pending_yolo {
            perm.set_yolo(true);
        }
        let session = ManagedSession {
            peer: peer.clone(),
            cwd: target_cwd,
            session_id: sid.clone(),
            short_id,
            agent_name,
            created_at: std::time::SystemTime::now(),
            last_active: Instant::now(),
            perm,
            project_context_sent: false,
        };
        let tag = session.tag();
        self.state.sessions.insert(key.clone(), session);
        self.publish_registry();

        Ok((sid, tag))
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
