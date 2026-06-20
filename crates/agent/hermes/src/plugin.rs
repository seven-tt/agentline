use std::sync::Arc;

use agentline_bridge::{AgentBackend, AgentBuildContext, AgentPlugin, AuthStatus, detect_command};
use async_trait::async_trait;
use serde::Deserialize;

pub struct HermesPlugin;

#[derive(Deserialize, Default)]
struct Cfg {
    #[serde(default)]
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    extra_env: Vec<(String, String)>,
    #[serde(default)]
    remove_env: Vec<String>,
}

#[async_trait]
impl AgentPlugin for HermesPlugin {
    fn id(&self) -> &'static str {
        "hermes"
    }
    fn display_name(&self) -> &'static str {
        "Hermes"
    }
    fn cli_command(&self) -> &'static str {
        "hermes"
    }
    fn install_command(&self) -> Option<&'static [&'static str]> {
        Some(&[
            "sh",
            "-c",
            "curl -fsSL https://hermes-agent.nousresearch.com/install.sh | bash",
        ])
    }
    fn detect(&self) -> (bool, Option<String>) {
        detect_command("hermes")
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
        let mut conf = crate::HermesConfig::default();
        if !cfg.command.is_empty() {
            conf.command = Some(cfg.command);
        }
        if !cfg.args.is_empty() {
            conf.args = Some(cfg.args);
        }
        conf.extra_env = ctx.proxy_env.clone();
        conf.extra_env.extend(cfg.extra_env);
        conf.remove_env = cfg.remove_env;
        conf.pid_file = ctx.pid_file.clone();
        crate::spawn(conf)
            .await
            .map(|b| Arc::new(b) as Arc<dyn AgentBackend>)
    }
}

pub fn plugin() -> Arc<dyn AgentPlugin> {
    Arc::new(HermesPlugin)
}
