//! Kiro CLI agent backend for agentline.
//!
//! Kiro CLI (AWS Kiro IDE's headless companion) **natively speaks ACP** via
//! `kiro-cli acp`, so this crate is a thin wrapper around
//! `agentline-agent-acp`.
//!
//! See <https://kiro.dev/docs/cli/acp/> for the official docs.
//!
//! # Prerequisites
//!
//! 1. Install Kiro CLI:
//!    ```bash
//!    curl -fsSL https://cli.kiro.dev/install | bash
//!    ```
//!    (Default install location is `~/.local/bin/kiro-cli`.)
//! 2. Log in interactively at least once with `kiro-cli` (e.g. start an
//!    interactive chat: `kiro-cli`).
//!
//! # Editor-aware path note
//!
//! Kiro's own docs warn that IDEs often don't inherit the user's `PATH`, so
//! `kiro-cli` may not be found unless you give an absolute path. When you
//! invoke this crate from a long-lived daemon, prefer
//! [`KiroConfig::command`] = `Some("/full/path/to/kiro-cli".into())` if the
//! launcher process's `PATH` doesn't include the install dir.

pub mod error;
pub use error::{Error, Result};

pub use agentline_agent_acp::AcpBackend;

#[derive(Debug, Clone, Default)]
pub struct KiroConfig {
    /// Override the launcher (default: `kiro-cli`). Pass an absolute path
    /// like `/Users/you/.local/bin/kiro-cli` if `kiro-cli` is not on the
    /// daemon's `PATH`.
    pub command: Option<String>,
    /// Override the launcher args entirely. If set, [`Self::agent_name`]
    /// is ignored. Default: `["acp"]`.
    pub args: Option<Vec<String>>,
    /// Use a specific Kiro agent configuration (translates to
    /// `--agent <name>`). Ignored if [`Self::args`] is set.
    pub agent_name: Option<String>,
    /// Extra env vars set on the child process.
    pub extra_env: Vec<(String, String)>,
    /// Env vars to strip from the child. Kiro has no known nested-session
    /// detection today, so the default is empty.
    pub remove_env: Vec<String>,
    pub pid_file: Option<std::path::PathBuf>,
}

impl KiroConfig {
    pub fn with_command(mut self, c: impl Into<String>) -> Self {
        self.command = Some(c.into());
        self
    }
    pub fn with_args(mut self, a: Vec<String>) -> Self {
        self.args = Some(a);
        self
    }
    pub fn with_agent_name(mut self, name: impl Into<String>) -> Self {
        self.agent_name = Some(name.into());
        self
    }
}

/// Spawn a Kiro agent (`kiro-cli acp [--agent <name>]`) and return a
/// ready-to-use `AcpBackend`.
pub async fn spawn(cfg: KiroConfig) -> Result<AcpBackend> {
    let command = cfg.command.unwrap_or_else(|| "kiro-cli".to_string());
    let args = cfg.args.unwrap_or_else(|| {
        let mut a = vec!["acp".to_string()];
        if let Some(name) = cfg.agent_name {
            a.push("--agent".to_string());
            a.push(name);
        }
        a
    });

    let acp_cfg = agentline_agent_acp::AcpBackendConfig {
        command,
        args,
        extra_env: cfg.extra_env,
        remove_env: cfg.remove_env,
        pid_file: cfg.pid_file,
    };
    Ok(AcpBackend::spawn(acp_cfg).await?)
}
