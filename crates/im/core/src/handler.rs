use agentline_bridge::Result;
use agentline_bridge::bridge::Bridge;
use agentline_bridge::permission::PermissionResponse;
use agentline_bridge::source::InboundHandler;
use agentline_bridge::types::{
    Command, ContentBlock, CwdResult, InboundPayload, ModeResult, PeerRef, PermissionResult,
    PromptResult, RoutedMessage, SessionId, UserContent, text_prompt,
};
use async_trait::async_trait;
use rust_i18n::t;
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::Mutex;

/// IM-specific state for the numbered-menu UI interaction.
#[derive(Debug)]
pub struct PendingSelection {
    pub peer: PeerRef,
    pub action: SelectionAction,
    pub choices: Vec<String>,
}

#[derive(Debug)]
pub enum SelectionAction {
    Close,
    Cd,
}

/// IM protocol inbound handler. Composes Bridge atomic operations for the
/// single-session-per-peer interaction model used by IM platforms.
///
/// Session creation is now the client's responsibility: this handler maintains
/// a `peer → SessionId` map and lazily creates a session (on the first prompt)
/// via `Bridge::new_session`.
pub struct ImInboundHandler {
    pending_selection: Mutex<Option<PendingSelection>>,
    sessions: Mutex<HashMap<String, SessionId>>,
}

/// Stable key for the `peer → SessionId` map.
fn peer_key(source_id: &str, peer: &PeerRef) -> String {
    format!("{source_id}|{}|{:?}", peer.user_id, peer.group_id)
}

impl ImInboundHandler {
    pub fn new() -> Self {
        Self {
            pending_selection: Mutex::new(None),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    async fn cached_session(&self, pkey: &str) -> Option<SessionId> {
        self.sessions.lock().await.get(pkey).cloned()
    }

    async fn forget_session(&self, pkey: &str) {
        self.sessions.lock().await.remove(pkey);
    }

    /// Resolve the session for this peer, creating one if needed.
    async fn ensure_session(
        &self,
        bridge: &Bridge,
        source_id: &str,
        peer: &PeerRef,
        pkey: &str,
    ) -> Result<SessionId> {
        if let Some(sid) = self.cached_session(pkey).await {
            return Ok(sid);
        }
        let sid = bridge
            .new_session(source_id.to_string(), peer.clone())
            .await?;
        self.sessions
            .lock()
            .await
            .insert(pkey.to_string(), sid.clone());
        Ok(sid)
    }

    async fn send_text(bridge: &Bridge, source_id: &str, peer: &PeerRef, text: &str) -> Result<()> {
        bridge.reply(source_id, peer, text, false).await
    }

    async fn send_markdown(
        bridge: &Bridge,
        source_id: &str,
        peer: &PeerRef,
        text: &str,
    ) -> Result<()> {
        bridge.reply(source_id, peer, text, true).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_command(
        &self,
        bridge: &Bridge,
        cmd: Command,
        peer: &PeerRef,
        source_id: &str,
        pkey: &str,
        cached: Option<SessionId>,
    ) -> Result<()> {
        match cmd {
            Command::Help => {
                Self::send_markdown(bridge, source_id, peer, &t!("bridge.help_text")).await?;
            }
            Command::Cd(path) => {
                let result = bridge.set_cwd(path.clone(), cached.as_ref()).await;
                let msg = match result {
                    CwdResult::Changed => {
                        t!("bridge.cd_success", path = path.display()).to_string()
                    }
                    CwdResult::SessionClosed { .. } => {
                        self.forget_session(pkey).await;
                        t!("bridge.cd_success", path = path.display()).to_string()
                    }
                    CwdResult::NotFound => {
                        t!("bridge.path_not_exist", path = path.display()).to_string()
                    }
                    CwdResult::NotADir => t!("bridge.not_a_dir", path = path.display()).to_string(),
                };
                Self::send_text(bridge, source_id, peer, &msg).await?;
            }
            Command::CdInteractive => {
                self.handle_cd_interactive(bridge, peer, source_id, cached.as_ref())
                    .await?;
            }
            Command::New => {
                if let Some(sid) = &cached {
                    bridge.close_session(sid).await?;
                    self.forget_session(pkey).await;
                }
                bridge.set_mode("safe", None).await;
                Self::send_text(bridge, source_id, peer, &t!("bridge.new_session")).await?;
            }
            Command::Close(target_id) => {
                self.handle_close(bridge, peer, source_id, target_id, pkey, cached.as_ref())
                    .await?;
            }
            Command::Cancel => match cached {
                Some(sid) => match bridge.cancel(&sid).await? {
                    Some(tag) => {
                        Self::send_text(
                            bridge,
                            source_id,
                            peer,
                            &t!("bridge.cancel_done", tag = tag),
                        )
                        .await?;
                    }
                    None => {
                        Self::send_text(bridge, source_id, peer, &t!("bridge.cancel_nothing"))
                            .await?;
                    }
                },
                None => {
                    Self::send_text(bridge, source_id, peer, &t!("bridge.cancel_nothing")).await?;
                }
            },
            Command::Agent(name) => {
                self.handle_agent(bridge, peer, source_id, name).await?;
            }
            Command::Sessions => {
                self.handle_sessions(bridge, peer, source_id, cached.as_ref())
                    .await?;
            }
            Command::Yolo => {
                let msg = match bridge.set_mode("yolo", cached.as_ref()).await {
                    ModeResult::Applied { tag } => t!("bridge.yolo_on", tag = tag).to_string(),
                    ModeResult::Pending => t!("bridge.yolo_on_next").to_string(),
                    ModeResult::NoSession => t!("bridge.yolo_on_next").to_string(),
                };
                Self::send_text(bridge, source_id, peer, &msg).await?;
            }
            Command::Safe => {
                let msg = match bridge.set_mode("safe", cached.as_ref()).await {
                    ModeResult::Applied { tag } => t!("bridge.yolo_off", tag = tag).to_string(),
                    ModeResult::Pending | ModeResult::NoSession => {
                        t!("bridge.safe_mode").to_string()
                    }
                };
                Self::send_text(bridge, source_id, peer, &msg).await?;
            }
            Command::YesToken | Command::NoToken | Command::SessionApprove => {
                self.handle_yes_no_session(bridge, cmd, peer, source_id, pkey, cached)
                    .await?;
            }
            Command::Plain(text) => {
                self.handle_plain_text(bridge, text, peer, source_id, pkey, cached)
                    .await?;
            }
        }
        Ok(())
    }

    async fn handle_content(
        &self,
        bridge: &Bridge,
        content: UserContent,
        peer: &PeerRef,
        source_id: &str,
        pkey: &str,
        cached: Option<SessionId>,
    ) -> Result<()> {
        if content.is_empty() {
            tracing::debug!("ignoring empty content");
            return Ok(());
        }
        let text = content.to_prompt_text();

        if let Some(sid) = &cached {
            if bridge.respond_elicitation(&text, None, sid).await {
                return Ok(());
            }
            if bridge.override_pending_perm(sid).await {
                Self::send_text(bridge, source_id, peer, &t!("bridge.override_cancel")).await?;
            }
        }

        if let Ok(n) = text.trim().parse::<usize>() {
            let sel = self.pending_selection.lock().await.take();
            if let Some(ps) = sel {
                return self
                    .handle_selection(bridge, ps, n, peer, source_id, pkey, cached)
                    .await;
            }
        }

        self.dispatch_prompt(
            bridge,
            peer,
            source_id,
            pkey,
            cached,
            content.to_content_blocks(),
        )
        .await?;
        Ok(())
    }

    async fn handle_yes_no_session(
        &self,
        bridge: &Bridge,
        cmd: Command,
        peer: &PeerRef,
        source_id: &str,
        pkey: &str,
        cached: Option<SessionId>,
    ) -> Result<()> {
        let resp = match &cmd {
            Command::YesToken => PermissionResponse::Once,
            Command::SessionApprove => PermissionResponse::Session,
            _ => PermissionResponse::Deny,
        };
        let text = match &cmd {
            Command::YesToken => "y",
            Command::NoToken => "n",
            Command::SessionApprove => "s",
            _ => "",
        };

        if let Some(sid) = &cached
            && bridge.respond_elicitation(text, None, sid).await
        {
            return Ok(());
        }

        if matches!(cmd, Command::NoToken) && self.pending_selection.lock().await.take().is_some() {
            Self::send_text(bridge, source_id, peer, &t!("bridge.cancelled")).await?;
            return Ok(());
        }

        let answer = match &cached {
            Some(sid) => bridge.respond_permission(resp, sid).await,
            None => PermissionResult::NoPending,
        };
        match answer {
            PermissionResult::Answered { effective } => {
                let body = match effective {
                    PermissionResponse::Once => t!("bridge.approved_once"),
                    PermissionResponse::Session => t!("bridge.approved_session"),
                    PermissionResponse::Deny => t!("bridge.denied"),
                };
                Self::send_text(bridge, source_id, peer, &body).await?;
            }
            PermissionResult::NoPending => {
                self.dispatch_prompt(bridge, peer, source_id, pkey, cached, text_prompt(text))
                    .await?;
            }
        }
        Ok(())
    }

    async fn handle_plain_text(
        &self,
        bridge: &Bridge,
        text: String,
        peer: &PeerRef,
        source_id: &str,
        pkey: &str,
        cached: Option<SessionId>,
    ) -> Result<()> {
        if let Some(sid) = &cached {
            if bridge.respond_elicitation(&text, None, sid).await {
                return Ok(());
            }
            if bridge.override_pending_perm(sid).await {
                Self::send_text(bridge, source_id, peer, &t!("bridge.override_cancel")).await?;
                self.dispatch_prompt(bridge, peer, source_id, pkey, cached, text_prompt(&text))
                    .await?;
                return Ok(());
            }
        }
        if let Ok(n) = text.trim().parse::<usize>() {
            let sel = self.pending_selection.lock().await.take();
            if let Some(ps) = sel {
                return self
                    .handle_selection(bridge, ps, n, peer, source_id, pkey, cached)
                    .await;
            }
        }
        self.dispatch_prompt(bridge, peer, source_id, pkey, cached, text_prompt(text))
            .await?;
        Ok(())
    }

    async fn handle_cd_interactive(
        &self,
        bridge: &Bridge,
        peer: &PeerRef,
        source_id: &str,
        cached: Option<&SessionId>,
    ) -> Result<()> {
        let (cwd, dirs) = bridge.get_cwd_and_subdirs().await;

        if dirs.is_empty() {
            let info = match cached {
                Some(sid) => bridge.session_info(sid).await,
                None => None,
            };
            let tag = info
                .map(|i| format!("[#{} {}]", i.short_id, i.agent_name))
                .unwrap_or_default();
            Self::send_text(
                bridge,
                source_id,
                peer,
                &t!("bridge.no_subdirs", tag = tag, cwd = cwd.display()),
            )
            .await?;
            return Ok(());
        }

        let mut msg = t!("bridge.select_cwd", cwd = cwd.display()).to_string();
        for (i, d) in dirs.iter().enumerate() {
            msg.push_str(&format!("{}. `{}`\n", i + 1, d.display()));
        }
        msg.push_str(&t!("bridge.reply_n_cancel"));
        Self::send_text(bridge, source_id, peer, &msg).await?;

        *self.pending_selection.lock().await = Some(PendingSelection {
            peer: peer.clone(),
            action: SelectionAction::Cd,
            choices: dirs
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
        });
        Ok(())
    }

    async fn handle_close(
        &self,
        bridge: &Bridge,
        peer: &PeerRef,
        source_id: &str,
        target_id: Option<u32>,
        pkey: &str,
        cached: Option<&SessionId>,
    ) -> Result<()> {
        let info = match cached {
            Some(sid) => bridge.session_info(sid).await,
            None => None,
        };

        if let Some(id) = target_id {
            if let (Some(sid), Some(info)) = (cached, &info)
                && info.short_id == id
            {
                let tag = bridge.close_session(sid).await?.unwrap_or_default();
                self.forget_session(pkey).await;
                Self::send_text(bridge, source_id, peer, &t!("bridge.close_done", tag = tag))
                    .await?;
                return Ok(());
            }
            Self::send_text(
                bridge,
                source_id,
                peer,
                &t!("bridge.session_not_found", id = id),
            )
            .await?;
            return Ok(());
        }

        match info {
            None => {
                Self::send_text(bridge, source_id, peer, &t!("bridge.no_active_session")).await?;
            }
            Some(info) => {
                let tag = format!("[#{} {}]", info.short_id, info.agent_name);
                let prompt =
                    t!("bridge.close_confirm", tag = tag, cwd = info.cwd.display()).to_string();
                Self::send_text(bridge, source_id, peer, &prompt).await?;
                *self.pending_selection.lock().await = Some(PendingSelection {
                    peer: peer.clone(),
                    action: SelectionAction::Close,
                    choices: vec![info.session_id.clone()],
                });
            }
        }
        Ok(())
    }

    async fn handle_agent(
        &self,
        bridge: &Bridge,
        peer: &PeerRef,
        source_id: &str,
        name: Option<String>,
    ) -> Result<()> {
        match name {
            None => match bridge.list_providers().await {
                Some((current, agents)) => {
                    let text = format_agent_list(&current, &agents);
                    bridge
                        .reply_agent_list(source_id, peer, &current, &agents, &text)
                        .await?;
                }
                None => {
                    Self::send_text(bridge, source_id, peer, &t!("bridge.agent_no_factory"))
                        .await?;
                }
            },
            Some(name) => match bridge.set_provider(&name).await {
                Ok(()) => {
                    Self::send_text(
                        bridge,
                        source_id,
                        peer,
                        &t!("bridge.agent_switched", agent = name),
                    )
                    .await?;
                }
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("not found") {
                        let agents = bridge.list_providers().await;
                        let usable: Vec<String> = agents
                            .iter()
                            .flat_map(|(_, list)| list.iter())
                            .filter(|a| {
                                a.status == agentline_bridge::AgentStatus::Ready
                                    || a.status == agentline_bridge::AgentStatus::Installed
                            })
                            .map(|a| a.name.clone())
                            .collect();
                        let list = usable.join(", ");
                        Self::send_text(
                            bridge,
                            source_id,
                            peer,
                            &t!("bridge.agent_not_found", name = name, available = list),
                        )
                        .await?;
                    } else {
                        Self::send_text(
                            bridge,
                            source_id,
                            peer,
                            &t!("bridge.agent_build_failed", name = name, error = msg),
                        )
                        .await?;
                    }
                }
            },
        }
        Ok(())
    }

    async fn handle_sessions(
        &self,
        bridge: &Bridge,
        peer: &PeerRef,
        source_id: &str,
        cached: Option<&SessionId>,
    ) -> Result<()> {
        let info = match cached {
            Some(sid) => bridge.session_info(sid).await,
            None => None,
        };
        let text = format_session_list(info.as_ref());
        bridge
            .reply_session_info(source_id, peer, info.as_ref(), &text)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_selection(
        &self,
        bridge: &Bridge,
        sel: PendingSelection,
        n: usize,
        peer: &PeerRef,
        source_id: &str,
        pkey: &str,
        cached: Option<SessionId>,
    ) -> Result<()> {
        if n == 0 {
            let len = sel.choices.len();
            *self.pending_selection.lock().await = Some(sel);
            Self::send_text(
                bridge,
                source_id,
                peer,
                &t!("bridge.reply_range", len = len),
            )
            .await?;
            return Ok(());
        }
        if n > sel.choices.len() {
            let len = sel.choices.len();
            Self::send_text(
                bridge,
                source_id,
                peer,
                &t!("bridge.invalid_number", n = n, len = len),
            )
            .await?;
            *self.pending_selection.lock().await = Some(sel);
            return Ok(());
        }
        let choice = &sel.choices[n - 1];
        match sel.action {
            SelectionAction::Cd => {
                let path = PathBuf::from(choice);
                let result = bridge.set_cwd(path.clone(), cached.as_ref()).await;
                let msg = match result {
                    CwdResult::Changed => {
                        t!("bridge.cd_success", path = path.display()).to_string()
                    }
                    CwdResult::SessionClosed { .. } => {
                        self.forget_session(pkey).await;
                        t!("bridge.cd_success", path = path.display()).to_string()
                    }
                    CwdResult::NotFound => {
                        t!("bridge.path_not_exist", path = path.display()).to_string()
                    }
                    CwdResult::NotADir => t!("bridge.not_a_dir", path = path.display()).to_string(),
                };
                Self::send_text(bridge, source_id, peer, &msg).await?;
            }
            SelectionAction::Close => {
                let tag = match &cached {
                    Some(sid) => bridge.close_session(sid).await?.unwrap_or_default(),
                    None => String::new(),
                };
                self.forget_session(pkey).await;
                Self::send_text(bridge, source_id, peer, &t!("bridge.close_done", tag = tag))
                    .await?;
            }
        }
        Ok(())
    }

    async fn dispatch_prompt(
        &self,
        bridge: &Bridge,
        peer: &PeerRef,
        source_id: &str,
        pkey: &str,
        cached: Option<SessionId>,
        content: Vec<ContentBlock>,
    ) -> Result<()> {
        let sid = match cached {
            Some(sid) => sid,
            None => self.ensure_session(bridge, source_id, peer, pkey).await?,
        };
        let result = match bridge.prompt(sid, content.clone()).await {
            // Our cached mapping outlived its session (e.g. `/agent` switched
            // backend and drained every session bridge-side) — forget it and
            // create a fresh one instead of silently going nowhere forever.
            Err(agentline_bridge::Error::SessionNotFound(_)) => {
                self.forget_session(pkey).await;
                let fresh = self.ensure_session(bridge, source_id, peer, pkey).await?;
                bridge.prompt(fresh, content).await?
            }
            other => other?,
        };
        match result {
            PromptResult::Queued { tag } => {
                Self::send_text(bridge, source_id, peer, &t!("bridge.queued", tag = tag)).await?;
            }
            PromptResult::Started => {}
        }
        Ok(())
    }
}

impl Default for ImInboundHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl InboundHandler for ImInboundHandler {
    async fn handle(&self, bridge: &Bridge, routed: RoutedMessage) -> Result<()> {
        let source_id = routed.source_id;
        let peer = routed.peer;
        let pkey = peer_key(&source_id, &peer);
        let cached = self.cached_session(&pkey).await;

        tracing::info!(
            source = %source_id,
            peer = %peer.user_id,
            "← im inbound message"
        );

        match routed.payload {
            InboundPayload::Command(cmd) => {
                self.execute_command(bridge, cmd, &peer, &source_id, &pkey, cached)
                    .await?;
            }
            InboundPayload::Content(content) => {
                self.handle_content(bridge, content, &peer, &source_id, &pkey, cached)
                    .await?;
            }
        }
        Ok(())
    }

    async fn invalidate_all_sessions(&self) {
        self.sessions.lock().await.clear();
    }
}

/// Formatted as Markdown — bold group headers and numbered lists render
/// cleanly via each adapter's `TextFormat::Markdown` path (card on
/// Feishu/DingTalk when configured, plain markdown elsewhere).
fn format_agent_list(current: &str, agents: &[agentline_bridge::AgentInfo]) -> String {
    use agentline_bridge::AgentStatus;

    let mut text = format!("🤖 **{}**: {current}", t!("bridge.agent_list_current"));

    let ready: Vec<&str> = agents
        .iter()
        .filter(|a| a.status == AgentStatus::Ready)
        .map(|a| a.name.as_str())
        .collect();
    let installed: Vec<&str> = agents
        .iter()
        .filter(|a| a.status == AgentStatus::Installed)
        .map(|a| a.name.as_str())
        .collect();
    let not_installed: Vec<&str> = agents
        .iter()
        .filter(|a| a.status == AgentStatus::NotInstalled)
        .map(|a| a.name.as_str())
        .collect();

    fn append_group(text: &mut String, label: &str, items: &[&str]) {
        if items.is_empty() {
            return;
        }
        text.push_str(&format!("\n\n**{label}**:"));
        for (i, name) in items.iter().enumerate() {
            text.push_str(&format!("\n{}. {name}", i + 1));
        }
    }

    append_group(&mut text, &t!("bridge.agent_list_ready"), &ready);
    append_group(&mut text, &t!("bridge.agent_list_installed"), &installed);
    append_group(
        &mut text,
        &t!("bridge.agent_list_not_installed"),
        &not_installed,
    );
    text
}

/// Format the `/sessions` reply from a session snapshot as a Markdown table.
/// Presentation lives in the IM layer now, not the bridge — each adapter
/// decides how to render `TextFormat::Markdown` (card vs. plain markdown).
fn format_session_list(info: Option<&agentline_bridge::SessionInfo>) -> String {
    use agentline_bridge::format::{fmt_ago, fmt_local};
    match info {
        None => t!("bridge.session_list_empty").to_string(),
        Some(s) => {
            let perm = if s.is_yolo {
                t!("bridge.yolo_label")
            } else {
                t!("bridge.safe_label")
            };
            format!(
                "### 📋 #{id} · {agent}\n\n\
                 | {field_h} | {value_h} |\n\
                 |---|---|\n\
                 | {sid_l} | `{sid}` |\n\
                 | {type_l} | {agent} |\n\
                 | {cwd_l} | `{cwd}` |\n\
                 | {start_l} | {started} |\n\
                 | {idle_l} | {idle} |\n\
                 | {perm_l} | {perm} |\n\
                 | {grant_l} | {grants} |",
                id = s.short_id,
                agent = s.agent_name,
                field_h = t!("bridge.session_header_field"),
                value_h = t!("bridge.session_header_value"),
                sid_l = t!("bridge.session_id_label"),
                sid = s.session_id,
                type_l = t!("bridge.session_type_label"),
                cwd_l = t!("bridge.session_cwd_label"),
                cwd = s.cwd.display(),
                start_l = t!("bridge.session_started_label"),
                started = fmt_local(s.created_at),
                idle_l = t!("bridge.session_idle_label"),
                idle = fmt_ago(s.idle_duration),
                perm_l = t!("bridge.session_perm_label"),
                perm = perm,
                grant_l = t!("bridge.session_grants_label"),
                grants = s.grant_summary,
            )
        }
    }
}
