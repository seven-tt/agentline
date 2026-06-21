use std::sync::Arc;

use agentline_bridge::{AgentBackend, AgentBuildContext, AgentPlugin, AuthStatus};
use async_trait::async_trait;
use serde::Deserialize;

pub struct AcpPlugin;

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
impl AgentPlugin for AcpPlugin {
    fn id(&self) -> &'static str {
        "acp"
    }
    fn display_name(&self) -> &'static str {
        "Generic ACP"
    }
    fn cli_command(&self) -> &'static str {
        ""
    }
    fn install_command(&self) -> Option<&'static [&'static str]> {
        None
    }
    fn detect(&self) -> (bool, Option<String>) {
        (false, None)
    }
    fn auth_status(&self) -> AuthStatus {
        AuthStatus::Ready
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
        if cfg.command.is_empty() {
            return Err(agentline_bridge::Error::other(
                "agent.backend = \"acp\" requires `agent.acp.command` in config",
            ));
        }
        let mut extra_env = ctx.proxy_env.clone();
        extra_env.extend(cfg.extra_env);
        let acp_cfg = crate::AcpBackendConfig {
            command: cfg.command,
            args: cfg.args,
            extra_env,
            remove_env: cfg.remove_env,
            pid_file: ctx.pid_file.clone(),
            mcp_servers: ctx.mcp_servers.clone(),
            ..Default::default()
        };
        crate::AcpBackend::spawn(acp_cfg)
            .await
            .map(|b| Arc::new(b) as Arc<dyn AgentBackend>)
            .map_err(|e| agentline_bridge::Error::other(e.to_string()))
    }
}

pub fn plugin() -> Arc<dyn AgentPlugin> {
    Arc::new(AcpPlugin)
}
