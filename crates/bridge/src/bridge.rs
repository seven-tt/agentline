use crate::agent::AgentBackend;
use crate::commands::{self, Command};
use crate::error::Result;
use crate::im::ImChannel;
use crate::permission::{
    AutoApprove, AutoApproveReason, PendingPerm, PermResponse, PermissionDecision,
    PermissionPolicy,
};
use crate::registry::{ImSnapshot, SessionRegistry, SessionSnapshot};
use crate::state::{
    ActiveSession, BridgeState, PendingElicit, PendingSelection, SelectionAction,
};
use crate::types::{
    AgentUpdate, ElicitField, ElicitFieldType, InboundMessage, MessageEvent, MessageKind, PeerRef,
    Project, SessionId, ToolKind,
};
use futures::StreamExt;
use rust_i18n::t;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::JoinHandle;

struct AbortOnDrop(JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}


#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub default_cwd: PathBuf,
    pub typing_interval: Duration,
    /// Close and recreate the agent session after this much idle time.
    /// `Duration::ZERO` disables the timeout.
    pub session_idle_timeout: Duration,
    /// Agent backend name shown in every message tag, e.g. "kimi" or "claude".
    pub agent_name: String,
    /// Configured project list. Users select a project with `/project <name>`;
    /// new sessions inject a project context as the first prompt.
    pub projects: Vec<Project>,
    /// When set, each new session creates a timestamped subdirectory under this
    /// path and uses it as cwd. Old session dirs beyond `keep=2` are pruned.
    pub session_base_dir: Option<PathBuf>,
    /// IM identifier (e.g. "wechat", "dingtalk"), used as the key in SessionRegistry.
    pub im_id: String,
    /// Shared session registry so the web dashboard can read session state.
    pub registry: Option<Arc<SessionRegistry>>,
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
            im_id: String::new(),
            registry: None,
        }
    }
}

pub struct Bridge {
    im: Arc<dyn ImChannel>,
    agent: Arc<dyn AgentBackend>,
    cfg: BridgeConfig,
    state: Arc<Mutex<BridgeState>>,
    prompt_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    start_next_lock: Arc<Mutex<()>>,
}

impl Bridge {
    pub fn new(im: Arc<dyn ImChannel>, agent: Arc<dyn AgentBackend>, cfg: BridgeConfig) -> Self {
        let state = BridgeState::new(cfg.default_cwd.clone());
        Self {
            im,
            agent,
            cfg,
            state: Arc::new(Mutex::new(state)),
            prompt_task: Arc::new(Mutex::new(None)),
            start_next_lock: Arc::new(Mutex::new(())),
        }
    }

    pub async fn run(self, mut inbound: mpsc::Receiver<InboundMessage>) -> Result<()> {
        while let Some(msg) = inbound.recv().await {
            if let Err(e) = self.handle_inbound(msg).await {
                tracing::error!(error=%e, "handle_inbound failed");
            }
        }
        let handle = {
            let mut guard = self.prompt_task.lock().await;
            guard.take()
        };
        if let Some(handle) = handle {
            let _ = handle.await;
        }
        // Drop any queued prompts that were never processed.
        self.state.lock().await.pending_prompts.clear();
        if let Some(active) = self.state.lock().await.current.take() {
            let _ = self.agent.close_session(active.session_id).await;
        }
        Ok(())
    }

    async fn handle_inbound(&self, msg: InboundMessage) -> Result<()> {
        // Build a text representation from any message kind (text, image, voice, file, video).
        let text = match &msg.kind {
            MessageKind::Text { text } => text.clone(),
            MessageKind::Image { local_path, caption } => {
                let mut parts = Vec::new();
                if let Some(path) = local_path {
                    parts.push(format!("![image]({})", path.display()));
                }
                if let Some(c) = caption {
                    parts.push(c.clone());
                }
                if parts.is_empty() {
                    tracing::debug!("ignoring inbound image without path or caption");
                    return Ok(());
                }
                parts.join("\n")
            }
            MessageKind::Voice { transcript, local_path } => {
                transcript.clone().unwrap_or_else(|| {
                    local_path
                        .as_ref()
                        .map(|p| format!("[voice]({})", p.display()))
                        .unwrap_or_default()
                })
            }
            MessageKind::File { local_path, name } => {
                format!("[file: {}]({})", name, local_path.display())
            }
            MessageKind::Video { local_path, caption } => {
                let mut parts = Vec::new();
                if let Some(path) = local_path {
                    parts.push(format!("[video]({})", path.display()));
                }
                if let Some(c) = caption {
                    parts.push(c.clone());
                }
                if parts.is_empty() {
                    tracing::debug!("ignoring inbound video without path or caption");
                    return Ok(());
                }
                parts.join("\n")
            }
        };

        if text.is_empty() {
            tracing::debug!("ignoring inbound message with empty text");
            return Ok(());
        }

        let cmd = commands::parse(&text);
        // Inbound message is INFO — it's the conversation record (the field
        // formatter truncates long content to one line).
        tracing::info!(peer = %msg.peer.user_id, text = %text, "← inbound message");

        match cmd {
            Command::Help => self.send(&msg.peer, &t!("bridge.help_text")).await?,

            Command::Cd(path) => self.handle_cd(&msg.peer, path).await?,
            Command::CdInteractive => self.handle_cd_interactive(&msg.peer).await?,

            Command::New => self.handle_new(&msg.peer).await?,

            Command::Stop(target_id) => self.handle_stop(&msg.peer, target_id).await?,

            Command::Sessions => self.handle_sessions(&msg.peer).await?,

            Command::Yolo => {
                let mut s = self.state.lock().await;
                s.pending_yolo = true;
                let msg_text = if let Some(a) = s.current.as_mut() {
                    a.perm.set_yolo(true);
                    let tag = a.tag();
                    t!("bridge.yolo_on", tag = tag)
                } else {
                    t!("bridge.yolo_on_next")
                };
                drop(s);
                self.send(&msg.peer, &msg_text).await?;
            }

            Command::Safe => {
                let mut s = self.state.lock().await;
                s.pending_yolo = false;
                if let Some(a) = s.current.as_mut() {
                    a.perm.set_yolo(false);
                    a.perm.clear_grants();
                    let tag = a.tag();
                    drop(s);
                    self.send(&msg.peer, &t!("bridge.yolo_off", tag = tag)).await?;
                } else {
                    drop(s);
                    self.send(&msg.peer, &t!("bridge.safe_mode")).await?;
                }
            }

            Command::YesToken | Command::NoToken | Command::SessionApprove => {
                let resp = match &cmd {
                    Command::YesToken => PermResponse::Once,
                    Command::SessionApprove => PermResponse::Session,
                    _ => PermResponse::Deny,
                };

                // First check pending elicitation (agent asked a question).
                let elicit = self.state.lock().await.pending_elicit.take();
                if let Some(pe) = elicit {
                    let response = parse_elicit_response(&text, pe.schema.as_deref());
                    let _ = self.agent.answer_elicitation(&pe.elicit_id, response).await;
                    return Ok(());
                }

                // NoToken also cancels a pending selection.
                if matches!(cmd, Command::NoToken) {
                    if self.state.lock().await.pending_selection.take().is_some() {
                        self.send(&msg.peer, &t!("bridge.cancelled")).await?;
                        return Ok(());
                    }
                }

                // Then check pending permission.
                let pending = self.state.lock().await.pending_perm.take();
                if let Some(pp) = pending {
                    let effective_resp = {
                        let s = self.state.lock().await;
                        let policy = s.current.as_ref().map(|a| &a.perm);
                        match policy {
                            Some(p) => p.effective_response(pp.tool_kind, resp, &pp.what),
                            None => resp,
                        }
                    };
                    // Record the grant before sending the response.
                    if matches!(effective_resp, PermResponse::Session) {
                        let mut s = self.state.lock().await;
                        if let Some(a) = s.current.as_mut() {
                            a.perm.apply_response(pp.tool_kind, effective_resp, &pp.what);
                        }
                    }
                    let _ = pp.responder.send(effective_resp);
                    let body = match effective_resp {
                        PermResponse::Once => &t!("bridge.approved_once"),
                        PermResponse::Session => &t!("bridge.approved_session"),
                        PermResponse::Deny => &t!("bridge.denied"),
                    };
                    self.send(&msg.peer, body).await?;
                } else {
                    // No pending request — treat as plain prompt.
                    self.dispatch_prompt(msg.peer, text).await?;
                }
            }

            Command::Plain(t) => {
                // Check pending elicitation first (agent asked a question).
                let elicit = self.state.lock().await.pending_elicit.take();
                if let Some(pe) = elicit {
                    let response = parse_elicit_response(&t, pe.schema.as_deref());
                    let _ = self.agent.answer_elicitation(&pe.elicit_id, response).await;
                    return Ok(());
                }
                // If a permission request is pending, the user's message is
                // context for re-evaluation — deny the current request, cancel
                // the running turn, and send the message as a fresh prompt so
                // the agent re-decides with this new input.
                let pending_perm = self.state.lock().await.pending_perm.take();
                if let Some(pp) = pending_perm {
                    let _ = pp.responder.send(PermResponse::Deny);
                    let sid = pp.session_id.clone();
                    self.stop_current_prompt(&sid).await;
                    self.send(&msg.peer, &t!("bridge.override_cancel")).await?;
                    // Bypass the queue — send directly as a new prompt in
                    // the same session (context is preserved).
                    self.dispatch_prompt(msg.peer, t).await?;
                    return Ok(());
                }
                // Check pending selection (/stop or /cd prompted the user to pick a number).
                if let Ok(n) = t.trim().parse::<usize>() {
                    let sel = self.state.lock().await.pending_selection.take();
                    if let Some(ps) = sel {
                        return self.handle_selection(ps, n, &msg.peer).await;
                    }
                }
                self.dispatch_prompt(msg.peer, t).await?;
            }
        }
        Ok(())
    }

    async fn handle_cd(&self, peer: &PeerRef, path: PathBuf) -> Result<()> {
        if !path.exists() {
            self.send(peer, &t!("bridge.path_not_exist", path = path.display()))
                .await?;
            return Ok(());
        }
        if !path.is_dir() {
            self.send(peer, &t!("bridge.not_a_dir", path = path.display()))
                .await?;
            return Ok(());
        }

        let to_close = {
            let mut s = self.state.lock().await;
            s.cwd = path.clone();
            match &s.current {
                Some(active) if active.cwd != path => Some(active.session_id.clone()),
                _ => None,
            }
        };
        if let Some(sid) = to_close {
            self.stop_current_prompt(&sid).await;
            let _ = self.agent.close_session(sid).await;
            self.state.lock().await.current = None;
            self.publish_registry().await;
        }

        self.send(peer, &t!("bridge.cd_success", path = path.display()))
            .await?;
        Ok(())
    }

    /// `/cd` with no argument: list subdirectories of current cwd and ask user to pick.
    async fn handle_cd_interactive(&self, peer: &PeerRef) -> Result<()> {
        let cwd = self.state.lock().await.cwd.clone();
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

        if dirs.is_empty() {
            let tag = self.state.lock().await.current.as_ref().map(|a| a.tag()).unwrap_or_default();
            self.send(peer, &t!("bridge.no_subdirs", tag = tag, cwd = cwd.display()))
                .await?;
            return Ok(());
        }

        let mut msg = t!("bridge.select_cwd", cwd = cwd.display()).to_string();
        for (i, d) in dirs.iter().enumerate() {
            msg.push_str(&format!("{}. `{}`\n", i + 1, d.display()));
        }
        msg.push_str(&t!("bridge.reply_n_cancel"));
        self.send(peer, &msg).await?;

        self.state.lock().await.pending_selection = Some(PendingSelection {
            peer: peer.clone(),
            action: SelectionAction::Cd,
            choices: dirs.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
        });
        Ok(())
    }

    async fn handle_new(&self, peer: &PeerRef) -> Result<()> {
        let sid = self.state.lock().await.current.as_ref().map(|a| a.session_id.clone());
        if let Some(ref sid) = sid {
            self.stop_current_prompt(sid).await;
        }
        let mut s = self.state.lock().await;
        let active = s.current.take();
        s.pending_yolo = false;
        s.project_context_sent = false;
        s.pending_prompts.clear();
        drop(s);
        if let Some(a) = active {
            let _ = self.agent.close_session(a.session_id).await;
        }
        self.publish_registry().await;
        self.send(peer, &t!("bridge.new_session")).await?;
        Ok(())
    }

    async fn handle_stop(&self, peer: &PeerRef, target_id: Option<u32>) -> Result<()> {
        let s = self.state.lock().await;
        let current = s.current.as_ref();

        // If a specific #N was given, try to match it directly.
        if let Some(id) = target_id {
            if let Some(a) = current.filter(|a| a.short_id == id) {
                let sid = a.session_id.clone();
                let tag = a.tag();
                drop(s);
                return self.do_stop(peer, &sid, &tag).await;
            }
            drop(s);
            self.send(peer, &t!("bridge.session_not_found", id = id)).await?;
            return Ok(());
        }

        // No ID given: if there's exactly one session, ask for confirmation.
        match current {
            None => {
                drop(s);
                self.send(peer, &t!("bridge.no_active_session")).await?;
            }
            Some(a) => {
                let short_id = a.short_id;
                let tag = a.tag();
                let sid_str = a.session_id.as_str().to_string();
                let cwd = a.cwd.display().to_string();
                drop(s);
                let prompt = format!(
                    "{}",
                    t!("bridge.stop_confirm", tag = tag, cwd = cwd)
                );
                self.send(peer, &prompt).await?;
                self.state.lock().await.pending_selection = Some(PendingSelection {
                    peer: peer.clone(),
                    action: SelectionAction::Stop,
                    choices: vec![sid_str],
                });
                let _ = short_id; // suppress warning
            }
        }
        Ok(())
    }

    async fn stop_current_prompt(&self, sid: &SessionId) {
        let _ = self.agent.cancel(sid).await;
        if let Some(handle) = self.prompt_task.lock().await.take() {
            handle.abort();
            let _ = handle.await;
        }
    }

    async fn do_stop(&self, peer: &PeerRef, sid: &SessionId, tag: &str) -> Result<()> {
        self.stop_current_prompt(sid).await;
        self.send(peer, &t!("bridge.stop_signal_sent", tag = tag)).await?;
        Ok(())
    }

    /// Execute the user's numeric choice for a pending selection.
    async fn handle_selection(
        &self,
        sel: PendingSelection,
        n: usize,
        peer: &PeerRef,
    ) -> Result<()> {
        if n == 0 {
            let len = sel.choices.len();
            self.state.lock().await.pending_selection = Some(sel);
            self.send(peer, &t!("bridge.reply_range", len = len)).await?;
            return Ok(());
        }
        if n > sel.choices.len() {
            let len = sel.choices.len();
            self.send(
                peer,
                &t!("bridge.invalid_number", n = n, len = len),
            )
            .await?;
            // Put the selection back so user can try again.
            self.state.lock().await.pending_selection = Some(sel);
            return Ok(());
        }
        let choice = &sel.choices[n - 1];
        match sel.action {
            SelectionAction::Cd => {
                self.handle_cd(peer, PathBuf::from(choice)).await?;
            }
            SelectionAction::Stop => {
                let sid = SessionId::new(choice.clone());
                let tag = self
                    .state
                    .lock()
                    .await
                    .current
                    .as_ref()
                    .filter(|a| a.session_id.as_str() == choice)
                    .map(|a| a.tag())
                    .unwrap_or_default();
                self.do_stop(peer, &sid, &tag).await?;
            }
        }
        Ok(())
    }

    async fn handle_sessions(&self, peer: &PeerRef) -> Result<()> {
        let s = self.state.lock().await;
        let text = match &s.current {
            None => t!("bridge.session_list_empty").to_string(),
            Some(a) => {
                let perm = if a.perm.is_yolo() {
                    t!("bridge.yolo_label")
                } else {
                    t!("bridge.safe_label")
                };
                let granted = a.perm.grant_summary();
                format!(
                    "## 📋 会话列表（共 1 个）\n\n\
                     ### `#{id}` · {agent}\n\
                     | 字段 | 值 |\n\
                     | :--- | :--- |\n\
                     | 🆔 Session ID | `{sid}` |\n\
                     | 🤖 类型 | {agent} |\n\
                     | 📁 工作目录 | `{cwd}` |\n\
                     | 🕐 启动时间 | {started} |\n\
                     | ⏱️ 空闲 | {idle} |\n\
                     | 🔐 权限 | {perm} |\n\
                     | ✅ session 级授权 | {granted} |",
                    id = a.short_id,
                    agent = a.agent_name,
                    sid = a.session_id.as_str(),
                    cwd = a.cwd.display(),
                    started = crate::format::fmt_local(a.created_at),
                    idle = crate::format::fmt_ago(a.last_active.elapsed()),
                    perm = perm,
                    granted = granted,
                )
            }
        };
        drop(s);
        self.send(peer, &text).await?;
        Ok(())
    }

    async fn dispatch_prompt(&self, peer: PeerRef, text: String) -> Result<()> {
        let was_empty = {
            let mut s = self.state.lock().await;
            let was_empty = s.pending_prompts.is_empty();
            s.pending_prompts.push_back((peer.clone(), text));
            was_empty
        };

        // Only notify if this is a subsequent message and a session already exists.
        if !was_empty {
            if let Some(tag) = self.state.lock().await.current.as_ref().map(|a| a.tag()) {
                self.send(
                    &peer,
                    &t!("bridge.queued", tag = tag),
                )
                .await?;
            }
        }

        start_next_if_idle(
            self.start_next_lock.clone(),
            self.prompt_task.clone(),
            self.state.clone(),
            self.agent.clone(),
            self.im.clone(),
            self.cfg.clone(),
        )
        .await
    }

    async fn send(&self, peer: &PeerRef, text: &str) -> Result<()> {
        self.im.send_text(peer, text).await
    }

    async fn publish_registry(&self) {
        if let Some(ref registry) = self.cfg.registry {
            let s = self.state.lock().await;
            let sessions = match &s.current {
                Some(a) => vec![SessionSnapshot {
                    id: a.session_id.as_str().to_string(),
                    user: a.peer.user_id.clone(),
                    active: true,
                    cwd: a.cwd.display().to_string(),
                }],
                None => vec![],
            };
            registry.update(
                &self.cfg.im_id,
                ImSnapshot {
                    healthy: true,
                    sessions,
                },
            );
        }
    }
}

async fn ensure_session_static(
    state: &Arc<Mutex<BridgeState>>,
    agent: &Arc<dyn AgentBackend>,
    cfg: &BridgeConfig,
    peer: &PeerRef,
) -> Result<(SessionId, String)> {
    let idle_timeout = cfg.session_idle_timeout;
    let agent_name = cfg.agent_name.clone();

    let (target_cwd, reuse) = {
        let mut s = state.lock().await;
        let target = s.cwd.clone();
        let reuse = s.current.as_ref().and_then(|a| {
            if a.cwd != target || a.peer.user_id != peer.user_id {
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
        if reuse.is_some() {
            if let Some(a) = s.current.as_mut() {
                a.last_active = Instant::now();
            }
        }
        (target, reuse)
    };

    if let Some((sid, tag)) = reuse {
        return Ok((sid, tag));
    }

    let to_close = state.lock().await.current.take();
    if let Some(a) = to_close {
        let _ = agent.close_session(a.session_id).await;
    }

    // When session_base_dir is set and cwd is still under the agent base,
    // create a fresh timestamped subdirectory for this session.
    let target_cwd = if let Some(ref base) = cfg.session_base_dir {
        if target_cwd == *base || target_cwd.starts_with(base) {
            let session_dir = base.join(format!("sess-{}", format_timestamp()));
            if let Err(e) = std::fs::create_dir_all(&session_dir) {
                tracing::error!(error=%e, dir=%session_dir.display(), "failed to create session dir");
            }
            state.lock().await.cwd = session_dir.clone();
            cleanup_old_sessions(base, 2);
            session_dir
        } else {
            target_cwd
        }
    } else {
        target_cwd
    };

    tracing::debug!(cwd=%target_cwd.display(), "creating agent session");
    let sid = agent.new_session(&target_cwd).await?;
    let short_id = state.lock().await.next_short_id();
    tracing::debug!(session=%sid.as_str(), id=short_id, "agent session ready");

    let pending_yolo = {
        let s = state.lock().await;
        s.pending_yolo
    };
    let mut perm = PermissionPolicy::new();
    if pending_yolo {
        perm.set_yolo(true);
    }
    let active = ActiveSession {
        peer: peer.clone(),
        cwd: target_cwd,
        session_id: sid.clone(),
        short_id,
        agent_name,
        created_at: std::time::SystemTime::now(),
        last_active: Instant::now(),
        perm,
    };
    let tag = active.tag();
    state.lock().await.current = Some(active);

    if let Some(ref registry) = cfg.registry {
        let s = state.lock().await;
        if let Some(a) = &s.current {
            registry.update(
                &cfg.im_id,
                ImSnapshot {
                    healthy: true,
                    sessions: vec![SessionSnapshot {
                        id: a.session_id.as_str().to_string(),
                        user: a.peer.user_id.clone(),
                        active: true,
                        cwd: a.cwd.display().to_string(),
                    }],
                },
            );
        }
    }

    Ok((sid, tag))
}

/// Dequeue the next pending prompt and start a task if the current one is idle.
fn start_next_if_idle(
    start_next_lock: Arc<Mutex<()>>,
    prompt_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    state: Arc<Mutex<BridgeState>>,
    agent: Arc<dyn AgentBackend>,
    im: Arc<dyn ImChannel>,
    cfg: BridgeConfig,
) -> impl std::future::Future<Output = Result<()>> + Send {
    async move {
    // Only one caller may dequeue + spawn at a time.
    let _guard = start_next_lock.lock().await;

    // If a task is still running, let it call us back when it finishes.
    {
        let guard = prompt_task.lock().await;
        if guard.as_ref().map(|h| !h.is_finished()).unwrap_or(false) {
            return Ok(());
        }
    }
    *prompt_task.lock().await = None;

    // Dequeue next message.
    let (peer, text) = {
        let mut s = state.lock().await;
        match s.pending_prompts.pop_front() {
            Some(v) => v,
            None => return Ok(()),
        }
    };

    let (sid, tag) = ensure_session_static(&state, &agent, &cfg, &peer).await?;

    // Inject project context as a prefix on the very first prompt of a new session.
    let text = {
        let mut s = state.lock().await;
        if !s.project_context_sent {
            s.project_context_sent = true;
            if !cfg.projects.is_empty() {
                let cwd = s.cwd.display().to_string();
                let mut ctx = String::new();
                ctx.push_str("Available projects:\n");
                for (i, p) in cfg.projects.iter().enumerate() {
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
            } else {
                text
            }
        } else {
            text
        }
    };

    let peer_t = peer.clone();
    let sid_t = sid.clone();
    let pt_c = prompt_task.clone();
    let state_c = state.clone();
    let agent_c = agent.clone();
    let im_c = im.clone();
    let cfg_c = cfg.clone();
    let snl_c = start_next_lock.clone();

    let handle = tokio::spawn(async move {
        if let Err(e) =
            run_prompt_task(im_c.clone(), agent_c.clone(), state_c.clone(), cfg_c.clone(), peer_t.clone(), sid_t, tag, text).await
        {
            tracing::error!(error=%e, "prompt task failed");
            state_c.lock().await.current = None;
            let _ = im_c
                .send_event(&peer_t, &MessageEvent::Error(e.to_string()))
                .await;
        }
        // Mark ourselves as finished so start_next_if_idle can dequeue the next message.
        let _ = pt_c.lock().await.take();
        let _ = start_next_if_idle(snl_c, pt_c, state_c, agent_c, im_c, cfg_c).await;
    });

    *prompt_task.lock().await = Some(handle);
    Ok(())
    }
}

async fn run_prompt_task(
    im: Arc<dyn ImChannel>,
    agent: Arc<dyn AgentBackend>,
    state: Arc<Mutex<BridgeState>>,
    cfg: BridgeConfig,
    peer: PeerRef,
    sid: SessionId,
    tag: String,
    text: String,
) -> Result<()> {
    let _typing_guard = AbortOnDrop({
        let im = im.clone();
        let peer = peer.clone();
        let interval = cfg.typing_interval;
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            tick.tick().await;
            loop {
                tick.tick().await;
                let _ = im.typing(&peer).await;
            }
        })
    });

    let target_cwd = state
        .lock()
        .await
        .current
        .as_ref()
        .map(|a| a.cwd.clone())
        .unwrap_or_else(|| cfg.default_cwd.clone());

    let result: Result<()> = async {
        tracing::debug!(session=%sid.as_str(), "→ sending prompt to agent");
        let mut stream = agent.prompt(&sid, &text).await?;
        tracing::debug!(session=%sid.as_str(), "agent stream opened; relaying updates");

        let mut thinking_buf = String::new();
        let mut thinking_start: Option<std::time::Instant> = None;
        let mut reply_buf = String::new();
        let mut reply_send_ok = true;
        let mut stream_started = false; // have we sent the session header for streaming?
        // Per-tool-call (kind, label). Populated from ToolCallStart updates (which
        // are not rendered on their own) so the single completion message can name
        // the file / command. The agent re-sends starts with progressively richer
        // input, so we keep the most descriptive label seen.
        let mut tool_meta: HashMap<String, (ToolKind, String, AutoApprove)> = HashMap::new();
        let mut next_tool_auto_approved = AutoApprove::No;

        while let Some(update) = stream.next().await {
            tracing::debug!(update=?update, "← agent update");

            // Flush thinking summary before user-visible events, but NOT
            // between tool calls — otherwise each thinking burst breaks the
            // WeChat tool-batch and consecutive tools are sent as separate messages.
            if !matches!(
                update,
                AgentUpdate::Thinking { .. }
                    | AgentUpdate::ToolCallStart { .. }
                    | AgentUpdate::ToolCallEnd { .. }
                    | AgentUpdate::ToolCallProgress { .. }
            ) && !thinking_buf.is_empty()
            {
                let elapsed = thinking_start.map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0);
                let chars = thinking_buf.chars().count();
                let summary = t!("bridge.thinking_summary", tag = tag, secs = format!("{:.1}", elapsed), chars = chars).to_string();
                if let Err(e) = im.send_event(&peer, &MessageEvent::PlainText(summary)).await {
                    tracing::error!(error=%e, "send thinking summary failed");
                }
                thinking_buf.clear();
                thinking_start = None;
            }

            let event = match update {
                AgentUpdate::Thinking { text } => {
                    thinking_buf.push_str(&text);
                    if thinking_start.is_none() {
                        thinking_start = Some(std::time::Instant::now());
                    }
                    continue;
                }

                AgentUpdate::AssistantText { delta, is_final } => {
                    if !delta.is_empty() {
                        // Prepend session header before the very first streaming chunk.
                        if !stream_started {
                            stream_started = true;
                            let header = format!("🤖 {tag} ");
                            if let Err(e) = im
                                .send_event(&peer, &MessageEvent::StreamChunk { text: header })
                                .await
                            {
                                tracing::error!(error=%e, "send stream header failed");
                            }
                        }
                        reply_buf.push_str(&delta);
                        if let Err(e) = im
                            .send_event(&peer, &MessageEvent::StreamChunk { text: delta })
                            .await
                        {
                            reply_send_ok = false;
                            tracing::error!(error=%e, "send_event StreamChunk failed");
                        }
                    }
                    if is_final {
                        if let Err(e) = im.send_event(&peer, &MessageEvent::StreamEnd).await {
                            reply_send_ok = false;
                            tracing::error!(error=%e, "send_event StreamEnd failed");
                        }
                    }
                    continue;
                }

                AgentUpdate::PermissionRequest { id, what: raw_what, danger, tool_kind } => {
                    let cwd_s = target_cwd.to_string_lossy();
                    let what = raw_what.replace(cwd_s.as_ref(), ".");

                    let decision = state
                        .lock()
                        .await
                        .current
                        .as_ref()
                        .map(|a| a.perm.evaluate(tool_kind, &what))
                        .unwrap_or(PermissionDecision::Ask);

                    match decision {
                        PermissionDecision::AutoApprove(reason) => {
                            if let Err(e) = agent.answer_permission(&sid, &id, true).await {
                                tracing::error!(error=%e, "auto-approve failed");
                            }
                            next_tool_auto_approved = match reason {
                                AutoApproveReason::Yolo => AutoApprove::Yolo,
                                AutoApproveReason::SessionGrant => AutoApprove::Session,
                            };
                            tracing::debug!(what=%what, ?reason, "auto-approve (silent)");
                            continue;
                        }
                        PermissionDecision::Ask => {
                            let (tx, rx) = oneshot::channel::<PermResponse>();
                            state.lock().await.pending_perm = Some(PendingPerm {
                                session_id: sid.clone(),
                                request_id: id.clone(),
                                responder: tx,
                                peer: peer.clone(),
                                tool_kind,
                                what: what.clone(),
                            });
                            let agent_c = agent.clone();
                            let sid_c = sid.clone();
                            let req_c = id.clone();
                            tokio::spawn(async move {
                                let resp = rx.await.unwrap_or(PermResponse::Deny);
                                let allow = !matches!(resp, PermResponse::Deny);
                                if let Err(e) =
                                    agent_c.answer_permission(&sid_c, &req_c, allow).await
                                {
                                    tracing::error!(error=%e, "answer_permission failed");
                                }
                            });
                            MessageEvent::PermissionRequest { id, what, danger, tag: tag.clone() }
                        }
                    }
                }

                AgentUpdate::ElicitInput { id, prompt, schema } => {
                    // Store pending elicit so handle_inbound can route the user's
                    // next reply back to the agent via agent.answer_elicitation().
                    state.lock().await.pending_elicit = Some(PendingElicit {
                        session_id: sid.clone(),
                        elicit_id: id.clone(),
                        peer: peer.clone(),
                        schema: schema.clone(),
                    });
                    // Emit the structured event — IM layer decides how to render.
                    if let Err(e) =
                        im.send_event(&peer, &MessageEvent::ElicitInput {
                            id,
                            prompt,
                            schema,
                        }).await
                    {
                        tracing::error!(error=%e, "send elicit question failed");
                    }
                    continue;
                }

                AgentUpdate::ModeChanged { mode_id } => {
                    MessageEvent::PlainText(t!("bridge.mode_changed", tag = tag, mode = mode_id).to_string())
                }

                AgentUpdate::SessionInfo { title } => {
                    MessageEvent::PlainText(format!("📋 {tag} {title}"))
                }

                AgentUpdate::Done => {
                    if !reply_buf.is_empty() {
                        // Outbound reply is INFO — the conversation record.
                        tracing::info!(
                            peer = %peer.user_id,
                            ok = reply_send_ok,
                            text = %reply_buf,
                            "→ outbound reply"
                        );
                    }
                    if let Err(e) = im.send_event(&peer, &MessageEvent::StreamEnd).await {
                        tracing::error!(error=%e, "send_event StreamEnd on Done failed");
                    }
                    if let Err(e) = im.send_event(&peer, &MessageEvent::Done).await {
                        tracing::error!(error=%e, "send_event Done failed");
                    }
                    break;
                }

                AgentUpdate::ToolCallStart { id, kind, label } => {
                    // Not rendered: just remember the call so its completion can
                    // name the file/command. Keep the most descriptive label seen.
                    let label = crate::format::strip_cwd_prefix(&label, &target_cwd);
                    let auto = std::mem::replace(&mut next_tool_auto_approved, AutoApprove::No);
                    match tool_meta.get_mut(&id) {
                        Some((k, l, a)) => {
                            if kind != ToolKind::Other {
                                *k = kind;
                            }
                            if label.len() > l.len() {
                                *l = label;
                            }
                            if !matches!(auto, AutoApprove::No) {
                                *a = auto;
                            }
                        }
                        None => {
                            tool_meta.insert(id, (kind, label, auto));
                        }
                    }
                    continue;
                }
                AgentUpdate::ToolCallProgress { id, output_chunk } => {
                    MessageEvent::ToolProgress { id, output: output_chunk }
                }
                AgentUpdate::ToolCallEnd { id, ok, summary } => {
                    // Name the completed tool by its remembered kind + arg. The
                    // entry is removed so a duplicate completion (the agent may
                    // signal one via status *and* via _meta) is reported once.
                    match tool_meta.remove(&id) {
                        Some((k, l, auto)) => {
                            let suffix = match auto {
                                AutoApprove::Session => &t!("bridge.session_approve"),
                                AutoApprove::Yolo => " · yolo授权",
                                AutoApprove::No => "",
                            };
                            let mut text = format!("{tag} {}{suffix}", crate::format::tool_label(k, &l));
                            // On failure, append the tool's own error output.
                            if !ok {
                                if let Some(err) = summary.filter(|s| !s.is_empty()) {
                                    text.push('\n');
                                    text.push_str(&crate::format::truncate(&err, 800));
                                }
                            }
                            MessageEvent::ToolEnd { id, ok, summary: Some(text) }
                        }
                        None => match summary {
                            Some(s) => MessageEvent::ToolEnd { id, ok, summary: Some(s) },
                            None => continue, // already reported, or never started
                        },
                    }
                }
                AgentUpdate::Plan { steps } => MessageEvent::Plan { steps },
                AgentUpdate::Error(msg) => {
                    MessageEvent::Error(format!("{tag} {msg}"))
                }
            };
            if let Err(e) = im.send_event(&peer, &event).await {
                tracing::error!(error=%e, "send_event failed");
            }
        }
        tracing::debug!(session=%sid.as_str(), "✓ prompt completed");
        Ok(())
    }
    .await;

    drop(_typing_guard);
    result
}

/// Human-readable name for a tool kind, used in `/sessions`.
fn format_timestamp() -> String {
    use std::time::SystemTime;
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Civil date from Unix day count (Howard Hinnant's algorithm, UTC).
    let z = secs as i64 / 86400 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let time = secs % 86400;
    let h = time / 3600;
    let min = (time % 3600) / 60;
    let s = time % 60;
    format!("{y:04}{m:02}{d:02}-{h:02}{min:02}{s:02}")
}

fn cleanup_old_sessions(base: &std::path::Path, keep: usize) {
    let entries = match std::fs::read_dir(base) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut dirs: Vec<String> = entries
        .filter_map(|e| {
            let e = e.ok()?;
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with("sess-") && e.path().is_dir() {
                Some(name)
            } else {
                None
            }
        })
        .collect();
    dirs.sort();
    if dirs.len() <= keep {
        return;
    }
    for name in &dirs[..dirs.len() - keep] {
        let p = base.join(name);
        tracing::debug!(dir=%p.display(), "removing old session directory");
        if let Err(e) = std::fs::remove_dir_all(&p) {
            tracing::warn!(error=%e, dir=%p.display(), "failed to remove old session dir");
        }
    }
}

/// Parse user reply into a structured `serde_json::Value` based on the
/// elicitation schema. If schema carries selectable options, numeric input
/// is resolved to the corresponding option value.
fn parse_elicit_response(text: &str, schema: Option<&[ElicitField]>) -> serde_json::Value {
    let text = text.trim();
    let Some(fields) = schema else {
        return serde_json::Value::String(text.to_string());
    };
    let Some(field) = fields.first() else {
        return serde_json::Value::String(text.to_string());
    };
    match &field.field_type {
        ElicitFieldType::SingleSelect { options } => {
            if let Ok(n) = text.parse::<usize>() {
                if n >= 1 && n <= options.len() {
                    return serde_json::Value::String(options[n - 1].value.clone());
                }
            }
            serde_json::Value::String(text.to_string())
        }
        ElicitFieldType::MultiSelect { options } => {
            let indices: Vec<usize> = text
                .split(',')
                .filter_map(|s| s.trim().parse::<usize>().ok())
                .collect();
            if !indices.is_empty() && indices.iter().all(|&i| i >= 1 && i <= options.len()) {
                let values: Vec<serde_json::Value> = indices
                    .iter()
                    .map(|&i| serde_json::Value::String(options[i - 1].value.clone()))
                    .collect();
                return serde_json::Value::Array(values);
            }
            serde_json::Value::String(text.to_string())
        }
        ElicitFieldType::Boolean => {
            let lower = text.to_lowercase();
            match lower.as_str() {
                "y" | "yes" | "true" | "是" | "1" => serde_json::Value::Bool(true),
                "n" | "no" | "false" | "否" | "0" => serde_json::Value::Bool(false),
                _ => serde_json::Value::String(text.to_string()),
            }
        }
        ElicitFieldType::Number { .. } => {
            if let Ok(n) = text.parse::<f64>() {
                serde_json::Number::from_f64(n)
                    .map(serde_json::Value::Number)
                    .unwrap_or_else(|| serde_json::Value::String(text.to_string()))
            } else {
                serde_json::Value::String(text.to_string())
            }
        }
        ElicitFieldType::Text { .. } => serde_json::Value::String(text.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentBackend;
    use crate::im::ImChannel;
    use crate::permission::PermissionDanger;
    use crate::types::{InboundMessage, MessageKind, ToolKind};
    use async_trait::async_trait;
    use futures::stream::{self, BoxStream, StreamExt};
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct MockIm {
        sent: Arc<StdMutex<Vec<(String, String)>>>,
    }

    #[async_trait]
    impl ImChannel for MockIm {
        async fn send_text(&self, to: &PeerRef, text: &str) -> Result<()> {
            self.sent
                .lock()
                .unwrap()
                .push((to.user_id.clone(), text.to_string()));
            Ok(())
        }
    }

    type SentLog = Arc<StdMutex<Vec<(String, String)>>>;

    impl MockIm {
        fn new() -> (Self, SentLog) {
            let sent = Arc::new(StdMutex::new(Vec::new()));
            (Self { sent: sent.clone() }, sent)
        }
    }

    #[derive(Default)]
    struct MockAgent {
        sessions: StdMutex<Vec<SessionId>>,
        cancelled: Arc<StdMutex<bool>>,
        permission_answers: Arc<StdMutex<Vec<(String, bool)>>>,
    }

    #[async_trait]
    impl AgentBackend for MockAgent {
        async fn new_session(&self, _cwd: &std::path::Path) -> Result<SessionId> {
            let sid = SessionId::new(format!("sess-{}", self.sessions.lock().unwrap().len()));
            self.sessions.lock().unwrap().push(sid.clone());
            Ok(sid)
        }

        async fn prompt<'a>(
            &'a self,
            _sid: &'a SessionId,
            text: &'a str,
        ) -> Result<BoxStream<'a, AgentUpdate>> {
            let mut updates = vec![
                AgentUpdate::ToolCallStart {
                    id: "t1".into(),
                    kind: ToolKind::Shell,
                    label: format!("echo {text}"),
                },
                // Tool starts aren't rendered on their own; the completion is the
                // message the user sees, labeled from the remembered start.
                AgentUpdate::ToolCallEnd {
                    id: "t1".into(),
                    ok: true,
                    summary: None,
                },
                AgentUpdate::AssistantText {
                    delta: format!("回声: {text}"),
                    is_final: true,
                },
                AgentUpdate::Done,
            ];
            if text.contains("@perm") {
                updates.insert(
                    0,
                    AgentUpdate::PermissionRequest {
                        id: "p1".into(),
                        what: "rm /tmp/x".into(),
                        danger: PermissionDanger::High,
                        tool_kind: ToolKind::Shell,
                    },
                );
            }
            Ok(stream::iter(updates).boxed())
        }

        async fn cancel(&self, _sid: &SessionId) -> Result<()> {
            *self.cancelled.lock().unwrap() = true;
            Ok(())
        }

        async fn answer_permission(
            &self,
            _sid: &SessionId,
            request_id: &str,
            allow: bool,
        ) -> Result<()> {
            self.permission_answers
                .lock()
                .unwrap()
                .push((request_id.to_string(), allow));
            Ok(())
        }

        async fn close_session(&self, _sid: SessionId) -> Result<()> {
            Ok(())
        }
    }

    fn make_peer() -> PeerRef {
        PeerRef {
            user_id: "user-A".into(),
            group_id: None,
            opaque: serde_json::Value::Null,
        }
    }

    fn make_text_msg(t: &str) -> InboundMessage {
        InboundMessage {
            peer: make_peer(),
            kind: MessageKind::Text { text: t.into() },
            received_at: std::time::SystemTime::now(),
        }
    }

    fn cfg_with_tmp() -> BridgeConfig {
        BridgeConfig {
            default_cwd: std::env::temp_dir(),
            typing_interval: Duration::from_secs(60),
            ..Default::default()
        }
    }

    fn init_test_locale() {
        rust_i18n::set_locale("zh-CN");
    }

    #[tokio::test]
    async fn happy_path_echo() {
        let (im, sent) = MockIm::new();
        let agent = Arc::new(MockAgent::default());
        let bridge = Bridge::new(Arc::new(im), agent, cfg_with_tmp());
        let (tx, rx) = mpsc::channel(8);
        tx.send(make_text_msg("hello")).await.unwrap();
        drop(tx);
        bridge.run(rx).await.unwrap();
        let sent = sent.lock().unwrap();
        let texts: Vec<_> = sent.iter().map(|(_, t)| t.as_str()).collect();
        assert!(texts.iter().any(|t| t.contains("echo hello")));
        assert!(texts.iter().any(|t| t.contains("回声: hello")));
    }

    #[tokio::test]
    async fn cd_invalid_path_reports() {
        init_test_locale();
        let (im, sent) = MockIm::new();
        let agent = Arc::new(MockAgent::default());
        let bridge = Bridge::new(Arc::new(im), agent, cfg_with_tmp());
        let (tx, rx) = mpsc::channel(8);
        tx.send(make_text_msg("/cd /nope/nada/123abc"))
            .await
            .unwrap();
        drop(tx);
        bridge.run(rx).await.unwrap();
        let sent = sent.lock().unwrap();
        assert!(sent.iter().any(|(_, t)| t.contains("路径不存在")));
    }

    #[tokio::test]
    async fn help_emits_help_text() {
        let (im, sent) = MockIm::new();
        let agent = Arc::new(MockAgent::default());
        let bridge = Bridge::new(Arc::new(im), agent, cfg_with_tmp());
        let (tx, rx) = mpsc::channel(8);
        tx.send(make_text_msg("/help")).await.unwrap();
        drop(tx);
        bridge.run(rx).await.unwrap();
        let sent = sent.lock().unwrap();
        assert!(sent.iter().any(|(_, t)| t.contains("Agentline")));
    }

    #[tokio::test]
    async fn yolo_auto_approves_permission() {
        init_test_locale();
        let (im, sent) = MockIm::new();
        let agent = Arc::new(MockAgent::default());
        let answers = agent.permission_answers.clone();
        let bridge = Bridge::new(Arc::new(im), agent, cfg_with_tmp());
        let (tx, rx) = mpsc::channel(8);
        // /yolo sets pending_yolo=true (no session needed).
        // @perm creates a session that inherits yolo=true, so the permission is
        // auto-approved without user interaction.
        tx.send(make_text_msg("/yolo")).await.unwrap();
        tx.send(make_text_msg("@perm please")).await.unwrap();
        drop(tx);
        bridge.run(rx).await.unwrap();
        let answers = answers.lock().unwrap();
        assert!(answers.iter().any(|(id, allow)| id == "p1" && *allow));
        let sent = sent.lock().unwrap();
        // yolo auto-approve is silent — no permission message sent to user.
        assert!(!sent.iter().any(|(_, t)| t.contains("需要授权")));
    }

    #[tokio::test]
    async fn unknown_slash_shows_help() {
        // Unknown slash-commands should show help, not be forwarded.
        let (im, sent) = MockIm::new();
        let agent = Arc::new(MockAgent::default());
        let bridge = Bridge::new(Arc::new(im), agent, cfg_with_tmp());
        let (tx, rx) = mpsc::channel(8);
        tx.send(make_text_msg("/compact")).await.unwrap();
        drop(tx);
        bridge.run(rx).await.unwrap();
        let sent = sent.lock().unwrap();
        assert!(sent.iter().any(|(_, t)| t.contains("Agentline")));
    }
}
