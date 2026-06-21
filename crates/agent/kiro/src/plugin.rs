use std::sync::Arc;

use agentline_bridge::{AgentBackend, AgentBuildContext, AgentPlugin, AuthStatus, detect_command};
use async_trait::async_trait;
use serde::Deserialize;

pub struct KiroPlugin;

#[derive(Deserialize, Default)]
struct Cfg {
    #[serde(default)]
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    agent_name: String,
    #[serde(default)]
    extra_env: Vec<(String, String)>,
    #[serde(default)]
    remove_env: Vec<String>,
}

#[async_trait]
impl AgentPlugin for KiroPlugin {
    fn id(&self) -> &'static str {
        "kiro"
    }
    fn display_name(&self) -> &'static str {
        "Kiro"
    }
    fn cli_command(&self) -> &'static str {
        "kiro-cli"
    }
    fn install_command(&self) -> Option<&'static [&'static str]> {
        Some(&["sh", "-c", "curl -fsSL https://cli.kiro.dev/install | bash"])
    }
    fn detect(&self) -> (bool, Option<String>) {
        detect_command("kiro-cli")
    }
    fn auth_status(&self) -> AuthStatus {
        if crate::config::is_logged_in() {
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
        let mut conf = crate::KiroConfig::default();
        if !cfg.command.is_empty() {
            conf.command = Some(cfg.command);
        }
        if !cfg.args.is_empty() {
            conf.args = Some(cfg.args);
        }
        if !cfg.agent_name.is_empty() {
            conf.agent_name = Some(cfg.agent_name);
        }
        conf.extra_env = ctx.proxy_env.clone();
        conf.extra_env.extend(cfg.extra_env);
        conf.remove_env = cfg.remove_env;
        conf.pid_file = ctx.pid_file.clone();
        conf.mcp_servers = ctx.mcp_servers.clone();
        crate::spawn(conf)
            .await
            .map(|b| Arc::new(b) as Arc<dyn AgentBackend>)
    }
}

pub fn plugin() -> Arc<dyn AgentPlugin> {
    Arc::new(KiroPlugin)
}
