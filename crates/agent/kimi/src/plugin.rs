use std::sync::Arc;

use agentline_bridge::{
    AgentBackend, AgentBuildContext, AgentPlugin, AuthStatus, CredentialInfo, CredentialUpdate,
    detect_command,
};
use async_trait::async_trait;
use serde::Deserialize;

pub struct KimiPlugin;

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
impl AgentPlugin for KimiPlugin {
    fn id(&self) -> &'static str {
        "kimi"
    }
    fn display_name(&self) -> &'static str {
        "Kimi Code"
    }
    fn cli_command(&self) -> &'static str {
        "kimi"
    }
    fn install_command(&self) -> Option<&'static [&'static str]> {
        Some(&[
            "sh",
            "-c",
            "curl -fsSL https://code.kimi.com/kimi-code/install.sh | bash",
        ])
    }
    fn detect(&self) -> (bool, Option<String>) {
        detect_command("kimi")
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
        let mut conf = crate::KimiConfig::default();
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
    Arc::new(KimiPlugin)
}
