use std::sync::Arc;

use agentline_bridge::{AgentBackend, AgentBuildContext, AgentPlugin, AuthStatus, detect_command};
use async_trait::async_trait;
use serde::Deserialize;

pub struct ClaudeCodePlugin;

#[derive(Deserialize)]
struct Cfg {
    #[serde(default)]
    version: String,
    #[serde(default)]
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    extra_env: Vec<(String, String)>,
    #[serde(default)]
    remove_env_extra: Vec<String>,
    #[serde(default = "default_true")]
    inject_settings_env: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Cfg {
    fn default() -> Self {
        Self {
            version: String::new(),
            command: String::new(),
            args: Vec::new(),
            extra_env: Vec::new(),
            remove_env_extra: Vec::new(),
            inject_settings_env: true,
        }
    }
}

#[async_trait]
impl AgentPlugin for ClaudeCodePlugin {
    fn id(&self) -> &'static str {
        "claude-code"
    }
    fn display_name(&self) -> &'static str {
        "Claude Code"
    }
    fn cli_command(&self) -> &'static str {
        "claude"
    }
    fn install_command(&self) -> Option<&'static [&'static str]> {
        Some(&["npm", "install", "-g", "@anthropic-ai/claude-code"])
    }
    fn detect(&self) -> (bool, Option<String>) {
        detect_command("claude")
    }
    fn auth_status(&self) -> AuthStatus {
        if crate::config::is_logged_in(&[]) {
            AuthStatus::Ready
        } else {
            AuthStatus::NeedsLogin
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
        let mut conf = crate::ClaudeCodeConfig::default();
        if !cfg.version.is_empty() {
            conf.version = cfg.version;
        }
        if !cfg.command.is_empty() {
            conf.command = Some(cfg.command);
        }
        if !cfg.args.is_empty() {
            conf.args = Some(cfg.args);
        }
        conf.extra_env = ctx.proxy_env.clone();
        conf.extra_env.extend(cfg.extra_env);
        conf.remove_env_extra = cfg.remove_env_extra;
        conf.inject_settings_env = cfg.inject_settings_env;
        conf.pid_file = ctx.pid_file.clone();
        conf.mcp_servers = ctx.mcp_servers.clone();
        crate::spawn(conf)
            .await
            .map(|b| Arc::new(b) as Arc<dyn AgentBackend>)
    }
}

pub fn plugin() -> Arc<dyn AgentPlugin> {
    Arc::new(ClaudeCodePlugin)
}
