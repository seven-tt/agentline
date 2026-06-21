use std::sync::Arc;

use agentline_bridge::{
    AgentBackend, AgentBuildContext, AgentPlugin, AuthStatus, CredentialInfo, CredentialUpdate,
    detect_command,
};
use async_trait::async_trait;
use serde::Deserialize;

pub struct OpencodePlugin;

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
impl AgentPlugin for OpencodePlugin {
    fn id(&self) -> &'static str {
        "opencode"
    }
    fn display_name(&self) -> &'static str {
        "OpenCode"
    }
    fn cli_command(&self) -> &'static str {
        "opencode"
    }
    fn install_command(&self) -> Option<&'static [&'static str]> {
        Some(&["npm", "install", "-g", "opencode-ai"])
    }
    fn detect(&self) -> (bool, Option<String>) {
        detect_command("opencode")
    }
    fn post_install(&self) {
        crate::config::post_install();
    }
    fn auth_status(&self) -> AuthStatus {
        if crate::config::is_logged_in() {
            AuthStatus::Ready
        } else {
            AuthStatus::NeedsLogin
        }
    }
    fn read_credential(&self) -> CredentialInfo {
        let (api_key, base_url) = crate::config::read_config();
        CredentialInfo { api_key, base_url }
    }
    fn sync_credential(&self, update: &CredentialUpdate) {
        crate::config::sync_config(update.api_key.as_deref(), update.base_url.as_deref());
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
        let mut conf = crate::OpencodeConfig::default();
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
        conf.mcp_servers = ctx.mcp_servers.clone();
        crate::spawn(conf)
            .await
            .map(|b| Arc::new(b) as Arc<dyn AgentBackend>)
    }
}

pub fn plugin() -> Arc<dyn AgentPlugin> {
    Arc::new(OpencodePlugin)
}
