use agentline_bridge::Result;
use agentline_bridge::bridge::Bridge;
use agentline_bridge::event::{OutboundEvent, TextFormat};
use agentline_bridge::permission::PermResponse;
use agentline_bridge::session::SessionKey;
use agentline_bridge::source::InboundHandler;
use agentline_bridge::types::{
    Command, CwdResult, InboundPayload, PeerRef, PermAnswerResult, PromptResult, RoutedMessage,
    SafeResult, UserContent, YoloResult,
};
use async_trait::async_trait;
use rust_i18n::t;
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

/// IM protocol inbound handler. Composes Bridge atomic operations for
/// the single-session interaction model used by IM platforms.
pub struct ImInboundHandler {
    pending_selection: Mutex<Option<PendingSelection>>,
}

impl ImInboundHandler {
    pub fn new() -> Self {
        Self {
            pending_selection: Mutex::new(None),
        }
    }

    async fn send_text(bridge: &Bridge, source_id: &str, peer: &PeerRef, text: &str) -> Result<()> {
        bridge
            .send_event(
                source_id,
                peer,
                &OutboundEvent::Text {
                    content: text.to_string(),
                    format: TextFormat::Plain,
                },
            )
            .await
    }

    async fn send_markdown(
        bridge: &Bridge,
        source_id: &str,
        peer: &PeerRef,
        text: &str,
    ) -> Result<()> {
        bridge
            .send_event(
                source_id,
                peer,
                &OutboundEvent::Text {
                    content: text.to_string(),
                    format: TextFormat::Markdown,
                },
            )
            .await
    }

    async fn execute_command(
        &self,
        bridge: &Bridge,
        cmd: Command,
        peer: &PeerRef,
        source_id: &str,
        key: &SessionKey,
    ) -> Result<()> {
        match cmd {
            Command::Help => {
                Self::send_markdown(bridge, source_id, peer, &t!("bridge.help_text")).await?;
            }
            Command::Cd(path) => {
                let result = bridge.set_cwd(path.clone(), key).await;
                let msg = match result {
                    CwdResult::Changed | CwdResult::SessionClosed { .. } => {
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
                self.handle_cd_interactive(bridge, peer, source_id, key)
                    .await?;
            }
            Command::New => {
                bridge.close_session(key).await?;
                bridge.set_yolo(false, key).await;
                Self::send_text(bridge, source_id, peer, &t!("bridge.new_session")).await?;
            }
            Command::Close(target_id) => {
                self.handle_close(bridge, peer, source_id, target_id, key)
                    .await?;
            }
            Command::Cancel => match bridge.cancel_prompt(key).await? {
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
                    Self::send_text(bridge, source_id, peer, &t!("bridge.cancel_nothing")).await?;
                }
            },
            Command::Agent(name) => {
                self.handle_agent(bridge, peer, source_id, name).await?;
            }
            Command::Sessions => {
                self.handle_sessions(bridge, peer, source_id, key).await?;
            }
            Command::Yolo => {
                let msg = match bridge.set_yolo(true, key).await {
                    YoloResult::Applied { tag } => t!("bridge.yolo_on", tag = tag).to_string(),
                    YoloResult::Pending => t!("bridge.yolo_on_next").to_string(),
                };
                Self::send_text(bridge, source_id, peer, &msg).await?;
            }
            Command::Safe => {
                let msg = match bridge.set_safe(key).await {
                    SafeResult::Applied { tag } => t!("bridge.yolo_off", tag = tag).to_string(),
                    SafeResult::NoSession => t!("bridge.safe_mode").to_string(),
                };
                Self::send_text(bridge, source_id, peer, &msg).await?;
            }
            Command::YesToken | Command::NoToken | Command::SessionApprove => {
                self.handle_yes_no_session(bridge, cmd, peer, source_id, key)
                    .await?;
            }
            Command::Plain(text) => {
                self.handle_plain_text(bridge, text, peer, source_id, key)
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
        key: &SessionKey,
    ) -> Result<()> {
        if content.is_empty() {
            tracing::debug!("ignoring empty content");
            return Ok(());
        }
        let text = content.to_prompt_text();

        if bridge.answer_elicitation(&text, None, key).await {
            return Ok(());
        }

        if bridge.override_pending_perm(key).await {
            Self::send_text(bridge, source_id, peer, &t!("bridge.override_cancel")).await?;
        }

        if let Ok(n) = text.trim().parse::<usize>() {
            let sel = self.pending_selection.lock().await.take();
            if let Some(ps) = sel {
                return self
                    .handle_selection(bridge, ps, n, peer, source_id, key)
                    .await;
            }
        }

        self.dispatch_prompt(bridge, key, peer.clone(), text, source_id.to_string())
            .await?;
        Ok(())
    }

    async fn handle_yes_no_session(
        &self,
        bridge: &Bridge,
        cmd: Command,
        peer: &PeerRef,
        source_id: &str,
        key: &SessionKey,
    ) -> Result<()> {
        let resp = match &cmd {
            Command::YesToken => PermResponse::Once,
            Command::SessionApprove => PermResponse::Session,
            _ => PermResponse::Deny,
        };
        let text = match &cmd {
            Command::YesToken => "y",
            Command::NoToken => "n",
            Command::SessionApprove => "s",
            _ => "",
        };

        if bridge.answer_elicitation(text, None, key).await {
            return Ok(());
        }

        if matches!(cmd, Command::NoToken) && self.pending_selection.lock().await.take().is_some() {
            Self::send_text(bridge, source_id, peer, &t!("bridge.cancelled")).await?;
            return Ok(());
        }

        match bridge.answer_permission(resp, key).await {
            PermAnswerResult::Answered { effective } => {
                let body = match effective {
                    PermResponse::Once => t!("bridge.approved_once"),
                    PermResponse::Session => t!("bridge.approved_session"),
                    PermResponse::Deny => t!("bridge.denied"),
                };
                Self::send_text(bridge, source_id, peer, &body).await?;
            }
            PermAnswerResult::NoPending => {
                self.dispatch_prompt(
                    bridge,
                    key,
                    peer.clone(),
                    text.to_string(),
                    source_id.to_string(),
                )
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
        key: &SessionKey,
    ) -> Result<()> {
        if bridge.answer_elicitation(&text, None, key).await {
            return Ok(());
        }
        if bridge.override_pending_perm(key).await {
            Self::send_text(bridge, source_id, peer, &t!("bridge.override_cancel")).await?;
            self.dispatch_prompt(bridge, key, peer.clone(), text, source_id.to_string())
                .await?;
            return Ok(());
        }
        if let Ok(n) = text.trim().parse::<usize>() {
            let sel = self.pending_selection.lock().await.take();
            if let Some(ps) = sel {
                return self
                    .handle_selection(bridge, ps, n, peer, source_id, key)
                    .await;
            }
        }
        self.dispatch_prompt(bridge, key, peer.clone(), text, source_id.to_string())
            .await?;
        Ok(())
    }

    async fn handle_cd_interactive(
        &self,
        bridge: &Bridge,
        peer: &PeerRef,
        source_id: &str,
        key: &SessionKey,
    ) -> Result<()> {
        let (cwd, dirs) = bridge.get_cwd_and_subdirs().await;

        if dirs.is_empty() {
            let info = bridge.session_info(key).await;
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
        key: &SessionKey,
    ) -> Result<()> {
        let info = bridge.session_info(key).await;

        if let Some(id) = target_id {
            if let Some(ref info) = info
                && info.short_id == id
            {
                let tag = bridge.close_session(key).await?.unwrap_or_default();
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
            None => match bridge.list_agents().await {
                Some((current, agents)) => {
                    let text = format_agent_list(&current, &agents);
                    Self::send_text(bridge, source_id, peer, &text).await?;
                }
                None => {
                    Self::send_text(bridge, source_id, peer, &t!("bridge.agent_no_factory"))
                        .await?;
                }
            },
            Some(name) => match bridge.switch_agent(&name).await {
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
                        let agents = bridge.list_agents().await;
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
        key: &SessionKey,
    ) -> Result<()> {
        let info = bridge.session_info(key).await;
        bridge
            .send_event(source_id, peer, &OutboundEvent::SessionList { info })
            .await
    }

    async fn handle_selection(
        &self,
        bridge: &Bridge,
        sel: PendingSelection,
        n: usize,
        peer: &PeerRef,
        source_id: &str,
        key: &SessionKey,
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
                let result = bridge.set_cwd(path.clone(), key).await;
                let msg = match result {
                    CwdResult::Changed | CwdResult::SessionClosed { .. } => {
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
                let tag = bridge.close_session(key).await?.unwrap_or_default();
                Self::send_text(bridge, source_id, peer, &t!("bridge.close_done", tag = tag))
                    .await?;
            }
        }
        Ok(())
    }

    async fn dispatch_prompt(
        &self,
        bridge: &Bridge,
        key: &SessionKey,
        peer: PeerRef,
        text: String,
        source_id: String,
    ) -> Result<()> {
        match bridge
            .send_prompt(key.clone(), peer.clone(), text, source_id.clone())
            .await?
        {
            PromptResult::Queued { tag } => {
                Self::send_text(bridge, &source_id, &peer, &t!("bridge.queued", tag = tag)).await?;
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
        let key = SessionKey::new(&source_id, &peer);

        tracing::info!(
            source = %source_id,
            peer = %peer.user_id,
            "← im inbound message"
        );

        match routed.payload {
            InboundPayload::Command(cmd) => {
                self.execute_command(bridge, cmd, &peer, &source_id, &key)
                    .await?;
            }
            InboundPayload::Content(content) => {
                self.handle_content(bridge, content, &peer, &source_id, &key)
                    .await?;
            }
        }
        Ok(())
    }
}

fn format_agent_list(current: &str, agents: &[agentline_bridge::AgentInfo]) -> String {
    use agentline_bridge::AgentStatus;

    let mut text = format!("🤖 {}: {current}", t!("bridge.agent_list_current"));

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
        text.push_str(&format!("\n\n{label}:"));
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
