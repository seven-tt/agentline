use std::sync::Arc;

use agentline_bridge::{
    AgentBackend, AgentBuildContext, AgentPlugin, AuthStatus, CredentialInfo, CredentialUpdate,
    detect_command,
};
use async_trait::async_trait;
use serde::Deserialize;

pub struct CodexPlugin;

#[derive(Deserialize)]
struct Cfg {
    #[serde(default)]
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    extra_env: Vec<(String, String)>,
    #[serde(default)]
    sandbox_mode: String,
    #[serde(default)]
    approval_mode: String,
    #[serde(default = "default_true")]
    skip_git_repo_check: bool,
    #[serde(default)]
    model: String,
}

fn default_true() -> bool {
    true
}

impl Default for Cfg {
    fn default() -> Self {
        Self {
            command: String::new(),
            args: Vec::new(),
            extra_env: Vec::new(),
            sandbox_mode: String::new(),
            approval_mode: String::new(),
            skip_git_repo_check: true,
            model: String::new(),
        }
    }
}

fn parse_sandbox(s: &str) -> Option<crate::SandboxMode> {
    match s {
        "read-only" => Some(crate::SandboxMode::ReadOnly),
        "workspace-write" => Some(crate::SandboxMode::WorkspaceWrite),
        "danger-full-access" => Some(crate::SandboxMode::DangerFullAccess),
        _ => None,
    }
}

fn parse_approval(s: &str) -> Option<crate::ApprovalMode> {
    match s {
        "never" => Some(crate::ApprovalMode::Never),
        "on-request" => Some(crate::ApprovalMode::OnRequest),
        "on-failure" => Some(crate::ApprovalMode::OnFailure),
        "untrusted" => Some(crate::ApprovalMode::Untrusted),
        _ => None,
    }
}

#[async_trait]
impl AgentPlugin for CodexPlugin {
    fn id(&self) -> &'static str {
        "codex"
    }
    fn display_name(&self) -> &'static str {
        "Codex"
    }
    fn cli_command(&self) -> &'static str {
        "codex"
    }
    fn install_command(&self) -> Option<&'static [&'static str]> {
        Some(&["npm", "install", "-g", "@openai/codex"])
    }
    fn detect(&self) -> (bool, Option<String>) {
        detect_command("codex")
    }
    fn auth_status(&self) -> AuthStatus {
        if crate::config::is_logged_in(&[]) {
            AuthStatus::Ready
        } else {
            AuthStatus::NeedsLogin
        }
    }
    fn read_credential(&self) -> CredentialInfo {
        CredentialInfo {
            api_key: crate::config::read_api_key(),
            ..Default::default()
        }
    }
    fn sync_credential(&self, update: &CredentialUpdate) {
        if let Some(ref k) = update.api_key {
            crate::config::sync_api_key(k);
        }
    }
    async fn build_backend(
        &self,
        ctx: &AgentBuildContext,
    ) -> agentline_bridge::Result<Arc<dyn AgentBackend>> {
        let cfg: Cfg = ctx
            .agent_config_value
            .clone()
            .try_into()
            .unwrap_or_default();
        let mut conf = crate::CodexConfig::default();
        if !cfg.command.is_empty() {
            conf.command = Some(cfg.command);
        }
        if !cfg.args.is_empty() {
            conf.args = Some(cfg.args);
        }
        conf.extra_env = ctx.proxy_env.clone();
        conf.extra_env.extend(cfg.extra_env);
        conf.sandbox_mode = parse_sandbox(&cfg.sandbox_mode).unwrap_or(conf.sandbox_mode);
        conf.approval_mode = parse_approval(&cfg.approval_mode).unwrap_or(conf.approval_mode);
        conf.skip_git_repo_check = cfg.skip_git_repo_check;
        if !cfg.model.is_empty() {
            conf.model = Some(cfg.model);
        }
        crate::CodexBackend::spawn(conf)
            .await
            .map(|b| Arc::new(b) as Arc<dyn AgentBackend>)
            .map_err(|e| agentline_bridge::Error::other(e.to_string()))
    }
}

pub fn plugin() -> Arc<dyn AgentPlugin> {
    Arc::new(CodexPlugin)
}
