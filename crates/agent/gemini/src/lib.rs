//! Gemini CLI agent backend for agentline.
//!
//! [google-gemini/gemini-cli](https://github.com/google-gemini/gemini-cli)
//! natively speaks ACP via `gemini --acp` (it depends on
//! `@agentclientprotocol/sdk` internally), so this crate is a thin wrapper
//! around `agentline-agent-acp`.
//!
//! # Prerequisites
//!
//! 1. Install Gemini CLI (one of):
//!    ```bash
//!    npm install -g @google/gemini-cli   # or npx @google/gemini-cli
//!    brew install gemini-cli             # macOS/Linux
//!    ```
//! 2. Authenticate (Google account flow, run interactively at least once):
//!    ```bash
//!    gemini
//!    ```

pub mod config;
pub mod plugin;
pub use plugin::plugin;

pub use agentline_agent_acp::AcpBackend;

#[derive(Debug, Clone, Default)]
pub struct GeminiConfig {
    /// Override the launcher (default: `gemini`).
    pub command: Option<String>,
    /// Override the launcher args (default: `["--acp"]`).
    pub args: Option<Vec<String>>,
    /// Extra env vars set on the child process.
    pub extra_env: Vec<(String, String)>,
    /// Env vars to strip from the child. Gemini has no known
    /// nested-session detection today, so the default is empty.
    pub remove_env: Vec<String>,
    pub pid_file: Option<std::path::PathBuf>,
    /// MCP servers to inject into the ACP session.
    pub mcp_servers: Vec<agentline_agent_acp::McpServer>,
}

impl GeminiConfig {
    pub fn with_command(mut self, c: impl Into<String>) -> Self {
        self.command = Some(c.into());
        self
    }
    pub fn with_args(mut self, a: Vec<String>) -> Self {
        self.args = Some(a);
        self
    }
}

/// Spawn a Gemini agent (`gemini --acp`) and return a ready-to-use `AcpBackend`.
pub async fn spawn(cfg: GeminiConfig) -> agentline_bridge::Result<AcpBackend> {
    let command = cfg.command.unwrap_or_else(|| "gemini".to_string());
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
