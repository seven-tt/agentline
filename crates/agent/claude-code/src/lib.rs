//! Claude Code agent backend for agentline.
//!
//! Thin convenience wrapper around `agentline-agent-acp` that:
//! - spawns `npx -y @zed-industries/claude-code-acp@<version>`
//! - scrubs the env vars Claude Code uses to detect nested sessions
//!   (`CLAUDECODE`, `CLAUDE_CODE_SESSION_ID`, ...)
//! - mirrors acp-cli's `ANTHROPIC_API_KEY` removal so OAuth setup tokens
//!   aren't misidentified as API keys by the underlying SDK
//! - optionally injects env vars from `~/.claude/settings.json` (covers
//!   proxy / non-default base URL configurations)
//!
//! Returns a plain `agentline_agent_acp::AcpBackend` — already an
//! `agentline_bridge::AgentBackend` impl, so you can pass it straight to a
//! `Bridge`.

pub mod config;
pub mod env_inject;
pub mod plugin;

pub use env_inject::inject_claude_settings_env;
pub use plugin::plugin;

pub use agentline_agent_acp::AcpBackend;

/// Default npm version of `@zed-industries/claude-code-acp` pinned by this crate.
pub const DEFAULT_VERSION: &str = "0.16.2";

/// Env vars Claude Code's claude-code-acp adapter uses to detect nested
/// sessions. **Always stripped** from the child env — overriding these would
/// make the child crash with "Claude Code cannot be launched inside another
/// Claude Code session." Users can only *add* more vars via
/// [`ClaudeCodeConfig::remove_env_extra`].
const DEFAULT_REMOVE_ENV: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    // Mirrors acp-cli: prevent OAuth setup tokens from being misidentified
    // as API keys by the underlying Claude Agent SDK.
    "ANTHROPIC_API_KEY",
    // Third-party gateway vars (kimi / ark / etc.) are commonly exported in the
    // launching shell. If they leak into the child, claude-code talks to that
    // gateway with its token instead of the user's `claude /login` OAuth
    // session — yielding `403 Request not allowed`. Strip them so the backend
    // falls back to the Keychain OAuth login. Users who genuinely want a proxy
    // can re-inject these via `agent.claude_code.extra_env` (applied *after*
    // removal) or via `~/.claude/settings.json`'s `env` block (read by the
    // claude CLI directly).
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_MODEL",
];

#[derive(Debug, Clone)]
pub struct ClaudeCodeConfig {
    /// npm version tag of `@zed-industries/claude-code-acp`. Defaults to
    /// [`DEFAULT_VERSION`].
    pub version: String,
    /// Override the launcher (default: `npx`). Pass an absolute path to a
    /// pre-installed `claude-code-acp` binary if you want to skip `npx`.
    pub command: Option<String>,
    /// Override the launcher args entirely. If set, `version` is ignored.
    pub args: Option<Vec<String>>,
    /// Extra env vars to set on the child process (overlays parent env).
    pub extra_env: Vec<(String, String)>,
    /// Additional env vars to strip from the child, on top of
    /// [`DEFAULT_REMOVE_ENV`]. The defaults are always applied — they cannot
    /// be opted out from config because doing so reliably kills the child.
    pub remove_env_extra: Vec<String>,
    /// If true (default), read `~/.claude/settings.json` and apply its `env`
    /// block to the current process before spawning, so the child inherits
    /// proxy / auth-token config (e.g. `ANTHROPIC_BASE_URL`).
    pub inject_settings_env: bool,
    /// Path to write the child PID for orphan cleanup by the tray.
    pub pid_file: Option<std::path::PathBuf>,
    /// MCP servers to inject into the ACP session.
    pub mcp_servers: Vec<agentline_agent_acp::McpServer>,
}

impl Default for ClaudeCodeConfig {
    fn default() -> Self {
        Self {
            version: DEFAULT_VERSION.to_string(),
            command: None,
            args: None,
            extra_env: Vec::new(),
            remove_env_extra: Vec::new(),
            inject_settings_env: true,
            pid_file: None,
            mcp_servers: Vec::new(),
        }
    }
}

impl ClaudeCodeConfig {
    pub fn with_version(mut self, v: impl Into<String>) -> Self {
        self.version = v.into();
        self
    }
}

/// Spawn a Claude Code agent and return a ready-to-use `AcpBackend`.
pub async fn spawn(cfg: ClaudeCodeConfig) -> agentline_bridge::Result<AcpBackend> {
    if cfg.inject_settings_env {
        let n = inject_claude_settings_env()?;
        if n > 0 {
            tracing::info!(injected = n, "applied env from ~/.claude/settings.json");
        }
    }

    let command = cfg.command.unwrap_or_else(|| "npx".to_string());
    let args = cfg.args.unwrap_or_else(|| {
        vec![
            "-y".into(),
            format!("@zed-industries/claude-code-acp@{}", cfg.version),
        ]
    });

    let mut remove_env: Vec<String> = DEFAULT_REMOVE_ENV.iter().map(|s| s.to_string()).collect();
    remove_env.extend(cfg.remove_env_extra);

    let acp_cfg = agentline_agent_acp::AcpBackendConfig {
        command,
        args,
        extra_env: cfg.extra_env,
        remove_env,
        pid_file: cfg.pid_file,
        mcp_servers: cfg.mcp_servers,
        ..Default::default()
    };
    AcpBackend::spawn(acp_cfg).await
}
