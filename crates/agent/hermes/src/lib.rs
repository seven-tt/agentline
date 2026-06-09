//! Hermes agent backend for agentline.
//!
//! [Nous Research Hermes Agent](https://hermes-agent.nousresearch.com/)
//! speaks ACP via `hermes acp`, so this crate is a thin wrapper
//! around `agentline-agent-acp`.
//!
//! # Prerequisites
//!
//! 1. Install Hermes:
//!    ```bash
//!    curl -fsSL https://hermes-agent.nousresearch.com/install.sh | bash
//!    ```
//! 2. Authenticate (OAuth portal flow, run interactively at least once):
//!    ```bash
//!    hermes setup --portal
//!    ```

pub mod error;
pub use error::{Error, Result};

pub use agentline_agent_acp::AcpBackend;

#[derive(Debug, Clone, Default)]
pub struct HermesConfig {
    /// Override the launcher (default: `hermes`).
    pub command: Option<String>,
    /// Override the launcher args (default: `["acp"]`).
    pub args: Option<Vec<String>>,
    /// Extra env vars set on the child process.
    pub extra_env: Vec<(String, String)>,
    /// Env vars to strip from the child.
    pub remove_env: Vec<String>,
    pub pid_file: Option<std::path::PathBuf>,
}

impl HermesConfig {
    pub fn with_command(mut self, c: impl Into<String>) -> Self {
        self.command = Some(c.into());
        self
    }
    pub fn with_args(mut self, a: Vec<String>) -> Self {
        self.args = Some(a);
        self
    }
}

/// Spawn a Hermes agent (`hermes acp`) and return a ready-to-use `AcpBackend`.
pub async fn spawn(cfg: HermesConfig) -> Result<AcpBackend> {
    let command = cfg.command.unwrap_or_else(|| "hermes".to_string());
    let args = cfg.args.unwrap_or_else(|| vec!["acp".to_string()]);

    let acp_cfg = agentline_agent_acp::AcpBackendConfig {
        command,
        args,
        extra_env: cfg.extra_env,
        remove_env: cfg.remove_env,
        pid_file: cfg.pid_file,
    };
    Ok(AcpBackend::spawn(acp_cfg).await?)
}
