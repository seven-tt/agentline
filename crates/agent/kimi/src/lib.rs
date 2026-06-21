//! Kimi Code CLI agent backend for agentline.
//!
//! Kimi Code CLI from Moonshot AI **natively speaks ACP** via `kimi acp`,
//! so this crate is a thin wrapper around `agentline-agent-acp`.
//!
//! # Prerequisites
//!
//! 1. Install Kimi Code CLI:
//!    ```bash
//!    pip install kimi-cli       # or pipx / uv tool install
//!    ```
//! 2. Log in once interactively:
//!    ```bash
//!    kimi /login
//!    ```
//!    (The ACP-mode `kimi acp` won't run an interactive login — it reuses
//!    credentials produced by the regular `kimi` CLI's `/login`.)
//!
//! See <https://moonshotai.github.io/kimi-cli/> for full docs.

pub mod plugin;
pub use plugin::plugin;

pub use agentline_agent_acp::AcpBackend;

pub mod config;
mod parser;
pub use parser::KimiToolCallParser;

#[derive(Debug, Clone, Default)]
pub struct KimiConfig {
    /// Override the launcher (default: `kimi`). Useful for `pipx`/`uvx`
    /// invocations or an absolute path.
    pub command: Option<String>,
    /// Override the launcher args (default: `["acp"]`).
    pub args: Option<Vec<String>>,
    /// Extra env vars set on the child process.
    pub extra_env: Vec<(String, String)>,
    /// Env vars to strip from the child. Kimi has no known nested-session
    /// detection today, so the default is empty.
    pub remove_env: Vec<String>,
    pub pid_file: Option<std::path::PathBuf>,
    /// MCP servers to inject into the ACP session.
    pub mcp_servers: Vec<agentline_agent_acp::McpServer>,
}

impl KimiConfig {
    pub fn with_command(mut self, c: impl Into<String>) -> Self {
        self.command = Some(c.into());
        self
    }
    pub fn with_args(mut self, a: Vec<String>) -> Self {
        self.args = Some(a);
        self
    }
}

/// Build the resolved (command, args) pair from a `KimiConfig`.
///
/// Extracted so it can be unit-tested without spawning a real process.
fn resolve_command_args(cfg: &KimiConfig) -> (String, Vec<String>) {
    let command = cfg.command.clone().unwrap_or_else(|| "kimi".to_string());
    let args = cfg.args.clone().unwrap_or_else(|| vec!["acp".to_string()]);
    (command, args)
}

/// Spawn a Kimi Code agent (`kimi acp`) and return a ready-to-use `AcpBackend`.
pub async fn spawn(cfg: KimiConfig) -> agentline_bridge::Result<AcpBackend> {
    let (command, args) = resolve_command_args(&cfg);

    let acp_cfg = agentline_agent_acp::AcpBackendConfig {
        command,
        args,
        extra_env: cfg.extra_env,
        remove_env: cfg.remove_env,
        pid_file: cfg.pid_file,
        mcp_servers: cfg.mcp_servers,
        parser: Some(std::sync::Arc::new(KimiToolCallParser::new())),
    };
    AcpBackend::spawn(acp_cfg).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_args_are_just_acp() {
        let cfg = KimiConfig::default();
        let (cmd, args) = resolve_command_args(&cfg);
        assert_eq!(cmd, "kimi");
        assert_eq!(args, vec!["acp"]);
    }

    #[test]
    fn default_args_never_contain_config_file() {
        // Regardless of whether ~/.kimi/config.toml exists on this machine,
        // the default args must NOT include --config-file — kimi v0.9+
        // does not support this flag and it causes a fatal startup error.
        let cfg = KimiConfig::default();
        let (_cmd, args) = resolve_command_args(&cfg);
        assert!(
            !args.iter().any(|a| a == "--config-file"),
            "default args must not contain --config-file, got: {args:?}"
        );
    }

    #[test]
    fn custom_command_is_respected() {
        let cfg = KimiConfig::default().with_command("/usr/local/bin/kimi");
        let (cmd, _args) = resolve_command_args(&cfg);
        assert_eq!(cmd, "/usr/local/bin/kimi");
    }

    #[test]
    fn custom_args_override_default() {
        let cfg = KimiConfig::default().with_args(vec!["acp".into(), "--login".into()]);
        let (_cmd, args) = resolve_command_args(&cfg);
        assert_eq!(args, vec!["acp", "--login"]);
    }
}
