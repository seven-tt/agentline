//! Qoder CLI agent backend for agentline.
//!
//! Qoder CLI **natively speaks ACP** via `qodercli --acp`, so this crate is
//! a thin wrapper around `agentline-agent-acp`.
//!
//! See <https://docs.qoder.com/en/cli/acp> for the official docs.
//!
//! # Prerequisites
//!
//! 1. Install Qoder CLI (see Qoder's official quickstart at
//!    <https://docs.qoder.com/>).
//! 2. Authenticate one of two ways:
//!    - Interactive: `qodercli login` (browser flow).
//!    - Non-interactive: set `QODER_PERSONAL_ACCESS_TOKEN` in
//!      [`QoderConfig::extra_env`] (or via the
//!      [`with_personal_access_token`](QoderConfig::with_personal_access_token)
//!      helper). Get a token at <https://qoder.com/account/integrations>.

pub mod config;
pub mod plugin;
pub use plugin::plugin;

pub use agentline_agent_acp::AcpBackend;

/// Env var Qoder CLI honors to skip interactive login.
pub const TOKEN_ENV: &str = "QODER_PERSONAL_ACCESS_TOKEN";

#[derive(Debug, Clone, Default)]
pub struct QoderConfig {
    /// Override the launcher (default: `qodercli`).
    pub command: Option<String>,
    /// Override the launcher args (default: `["--acp"]`).
    pub args: Option<Vec<String>>,
    /// Extra env vars set on the child process.
    pub extra_env: Vec<(String, String)>,
    /// Env vars to strip from the child. Qoder has no known nested-session
    /// detection today, so the default is empty.
    pub remove_env: Vec<String>,
    pub pid_file: Option<std::path::PathBuf>,
    /// MCP servers to inject into the ACP session.
    pub mcp_servers: Vec<agentline_agent_acp::McpServer>,
}

impl QoderConfig {
    pub fn with_command(mut self, c: impl Into<String>) -> Self {
        self.command = Some(c.into());
        self
    }
    pub fn with_args(mut self, a: Vec<String>) -> Self {
        self.args = Some(a);
        self
    }
    /// Convenience: inject the personal-access-token env var so the child
    /// can authenticate non-interactively.
    pub fn with_personal_access_token(mut self, token: impl Into<String>) -> Self {
        self.extra_env.push((TOKEN_ENV.to_string(), token.into()));
        self
    }
}

/// Spawn a Qoder CLI agent (`qodercli --acp`) and return a ready-to-use `AcpBackend`.
pub async fn spawn(cfg: QoderConfig) -> agentline_bridge::Result<AcpBackend> {
    let command = cfg.command.unwrap_or_else(|| "qodercli".to_string());
    let args = cfg.args.unwrap_or_else(|| vec!["--acp".to_string()]);

    let acp_cfg = agentline_agent_acp::AcpBackendConfig {
        command,
        args,
        extra_env: cfg.extra_env,
        remove_env: cfg.remove_env,
        pid_file: cfg.pid_file,
        mcp_servers: cfg.mcp_servers,
        ..Default::default()
    };
    AcpBackend::spawn(acp_cfg).await
}
