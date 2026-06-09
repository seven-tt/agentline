//! OpenCode CLI agent backend for agentline.
//!
//! OpenCode CLI ([sst/opencode](https://github.com/sst/opencode)) **natively
//! speaks ACP** via `opencode acp` — it uses the standard
//! `@agentclientprotocol/sdk` under the hood — so this crate is a thin
//! wrapper around `agentline-agent-acp`.
//!
//! # Prerequisites
//!
//! 1. Install OpenCode CLI (one of):
//!    ```bash
//!    curl -fsSL https://opencode.ai/install | bash
//!    npm i -g opencode-ai@latest
//!    brew install anomalyco/tap/opencode
//!    ```
//! 2. Authenticate one of the supported providers:
//!    ```bash
//!    opencode auth login
//!    ```

pub mod error;
pub use error::{Error, Result};

pub use agentline_agent_acp::AcpBackend;

#[derive(Debug, Clone, Default)]
pub struct OpencodeConfig {
    /// Override the launcher (default: `opencode`).
    pub command: Option<String>,
    /// Override the launcher args (default: `["acp"]`).
    pub args: Option<Vec<String>>,
    /// Extra env vars set on the child process.
    pub extra_env: Vec<(String, String)>,
    /// Env vars to strip from the child. OpenCode has no known
    /// nested-session detection today, so the default is empty.
    pub remove_env: Vec<String>,
    pub pid_file: Option<std::path::PathBuf>,
}

impl OpencodeConfig {
    pub fn with_command(mut self, c: impl Into<String>) -> Self {
        self.command = Some(c.into());
        self
    }
    pub fn with_args(mut self, a: Vec<String>) -> Self {
        self.args = Some(a);
        self
    }
}

/// Spawn an OpenCode agent (`opencode acp`) and return a ready-to-use `AcpBackend`.
pub async fn spawn(cfg: OpencodeConfig) -> Result<AcpBackend> {
    let command = cfg.command.unwrap_or_else(|| "opencode".to_string());
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
