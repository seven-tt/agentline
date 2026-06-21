use crate::error::Result;
use crate::types::{AgentSessionId, AgentUpdate, ContentBlock};
use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A coding agent that can run prompts inside a project working directory.
///
/// Each adapter (ACP, Codex, Kimi, ...) maps its native protocol onto these
/// methods. The `AgentSessionId` newtype is the same shape for everyone (a string)
/// to keep the trait object-safe; adapters encode any internal state behind it.
#[async_trait]
pub trait AgentBackend: Send + Sync + 'static {
    /// Open a fresh session with `cwd` as the working directory.
    async fn new_session(&self, cwd: &Path) -> Result<AgentSessionId>;

    /// Send a user prompt (ACP content blocks) to a session and return a stream
    /// of update frames. The stream ends when the agent finishes the turn (or
    /// errors).
    async fn prompt<'a>(
        &'a self,
        sid: &'a AgentSessionId,
        content: &'a [ContentBlock],
    ) -> Result<BoxStream<'a, AgentUpdate>>;

    /// Cancel the in-flight prompt of `sid`. The active stream should
    /// terminate (with a final `AgentUpdate::Done` or `Error`) shortly after.
    async fn cancel(&self, sid: &AgentSessionId) -> Result<()>;

    /// Reply to a permission request previously surfaced via
    /// `AgentUpdate::PermissionRequest`.
    async fn respond_permission(
        &self,
        sid: &AgentSessionId,
        request_id: &str,
        allow: bool,
    ) -> Result<()>;

    /// Tear down a session. After this, the AgentSessionId is invalid.
    async fn close_session(&self, sid: AgentSessionId) -> Result<()>;

    /// Reply to an elicitation request previously surfaced via
    /// `AgentUpdate::ElicitInput`.  Default is a no-op for backends that do
    /// not support elicitation.
    async fn respond_elicitation(&self, _elicit_id: &str, _response: Value) -> Result<()> {
        Ok(())
    }

    /// Terminate the backend and any child processes it spawned. Called on
    /// graceful shutdown (e.g. Ctrl-C). Default is a no-op.
    async fn shutdown(&self) {}
}

/// Agent installation / readiness status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStatus {
    /// Currently the active agent (has a running session or is the configured default).
    Ready,
    /// Binary found on PATH but not the active agent.
    Installed,
    /// Binary not found on PATH.
    NotInstalled,
}

/// Agent name paired with its installation status.
#[derive(Debug, Clone)]
pub struct AgentInfo {
    pub name: String,
    pub status: AgentStatus,
}

/// Authentication / readiness state returned by `AgentPlugin::auth_status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthStatus {
    Ready,
    NeedsLogin,
}

/// Unified credential snapshot read from an agent's native config.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct CredentialInfo {
    pub api_key: String,
    pub base_url: String,
}

/// Inbound credential update applied via `AgentPlugin::sync_credential`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct CredentialUpdate {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
}

/// Context passed to `AgentPlugin::build_backend` at startup.
pub struct AgentBuildContext {
    pub proxy_env: Vec<(String, String)>,
    pub pid_file: Option<PathBuf>,
    /// The agent's own `[agent.<id>]` section from config.toml, or `Value::Table({})` if absent.
    pub agent_config_value: toml::Value,
    /// MCP servers to inject into every new ACP session.
    pub mcp_servers: Vec<agent_client_protocol::McpServer>,
}

/// Unified facade every agent crate implements.
///
/// Each method has a self-contained default so crates only override what they need.
#[async_trait]
pub trait AgentPlugin: Send + Sync + 'static {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn cli_command(&self) -> &'static str;

    fn install_command(&self) -> Option<&'static [&'static str]>;

    /// Check whether the CLI binary is installed and return its version.
    fn detect(&self) -> (bool, Option<String>);

    /// Called once after a successful install to seed default config.
    fn post_install(&self) {}

    fn auth_status(&self) -> AuthStatus;

    fn read_credential(&self) -> CredentialInfo {
        CredentialInfo::default()
    }

    fn sync_credential(&self, _update: &CredentialUpdate) {}

    async fn build_backend(&self, ctx: &AgentBuildContext) -> Result<Arc<dyn AgentBackend>>;
}

/// Ordered list of all registered agent plugins.
pub type AgentPluginRegistry = Vec<Arc<dyn AgentPlugin>>;

/// `AgentFactory` implementation backed by a `AgentPluginRegistry`.
///
/// The `agent_section` is the full `[agent]` TOML table; at `build()` time
/// the factory slices out `agent_section[name]` and hands it to the plugin as
/// its `AgentBuildContext::agent_config_value`.
pub struct PluginAgentFactory {
    plugins: AgentPluginRegistry,
    proxy_env: Vec<(String, String)>,
    pid_file: Option<PathBuf>,
    /// The full `[agent]` section from config.toml as a TOML value.
    agent_section: toml::Value,
    mcp_servers: Vec<agent_client_protocol::McpServer>,
}

impl PluginAgentFactory {
    pub fn new(
        plugins: AgentPluginRegistry,
        proxy_env: Vec<(String, String)>,
        pid_file: Option<PathBuf>,
        agent_section: toml::Value,
        mcp_servers: Vec<agent_client_protocol::McpServer>,
    ) -> Self {
        Self {
            plugins,
            proxy_env,
            pid_file,
            agent_section,
            mcp_servers,
        }
    }

    fn make_ctx(&self, agent_id: &str) -> AgentBuildContext {
        let agent_config_value = self
            .agent_section
            .get(agent_id)
            .cloned()
            .unwrap_or_else(|| toml::Value::Table(Default::default()));
        AgentBuildContext {
            proxy_env: self.proxy_env.clone(),
            pid_file: self.pid_file.clone(),
            agent_config_value,
            mcp_servers: self.mcp_servers.clone(),
        }
    }
}

#[async_trait]
impl AgentFactory for PluginAgentFactory {
    fn available(&self) -> Vec<String> {
        self.plugins.iter().map(|p| p.id().to_string()).collect()
    }

    fn available_with_status(&self, _current_agent: &str) -> Vec<AgentInfo> {
        self.plugins
            .iter()
            .map(|p| {
                let (installed, _version) = p.detect();
                let status = match (installed, p.auth_status()) {
                    (false, _) => AgentStatus::NotInstalled,
                    (true, AuthStatus::Ready) => AgentStatus::Ready,
                    (true, AuthStatus::NeedsLogin) => AgentStatus::Installed,
                };
                AgentInfo {
                    name: p.id().to_string(),
                    status,
                }
            })
            .collect()
    }

    async fn build(&self, name: &str) -> Result<Arc<dyn AgentBackend>> {
        let ctx = self.make_ctx(name);

        // Known plugin → delegate directly.
        if let Some(plugin) = self.plugins.iter().find(|p| p.id() == name) {
            return plugin.build_backend(&ctx).await;
        }

        // Unknown name but [agent.<name>] has a `command` → treat as generic ACP.
        // This lets users define multiple custom ACP backends without changing code:
        //   [agent.my-agent1]
        //   command = "my-acp-server"
        let has_command = ctx
            .agent_config_value
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false);

        if has_command && let Some(acp) = self.plugins.iter().find(|p| p.id() == "acp") {
            return acp.build_backend(&ctx).await;
        }

        Err(crate::error::Error::other(format!(
            "unknown agent backend: {name:?} \
             (add a [agent.{name}] section with `command = \"...\"` for a custom ACP agent)"
        )))
    }
}

/// Factory for building agent backends by name at runtime (used by `/agent`).
#[async_trait]
pub trait AgentFactory: Send + Sync + 'static {
    /// List available agent backend names.
    fn available(&self) -> Vec<String>;

    /// List agents with installation status. The default implementation marks
    /// `current_agent` as [`AgentStatus::Ready`] and everything else as
    /// [`AgentStatus::Installed`].
    fn available_with_status(&self, current_agent: &str) -> Vec<AgentInfo> {
        self.available()
            .into_iter()
            .map(|name| {
                let status = if name == current_agent {
                    AgentStatus::Ready
                } else {
                    AgentStatus::Installed
                };
                AgentInfo { name, status }
            })
            .collect()
    }

    /// Build an agent backend by name.
    async fn build(&self, name: &str) -> Result<Arc<dyn AgentBackend>>;
}

/// Detect whether a CLI binary is installed and return its version string.
///
/// Used by `AgentPlugin::detect()` implementations — centralised here so
/// every plugin uses the same logic.  An empty `cmd` always returns
/// `(false, None)`.
pub fn detect_command(cmd: &str) -> (bool, Option<String>) {
    if cmd.is_empty() {
        return (false, None);
    }
    let installed = which::which(cmd).is_ok();
    let version = if installed {
        std::process::Command::new(cmd)
            .arg("--version")
            .output()
            .ok()
            .and_then(|o| {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if s.is_empty() {
                    String::from_utf8_lossy(&o.stderr)
                        .lines()
                        .next()
                        .map(|l| l.trim().to_string())
                } else {
                    Some(
                        s.lines()
                            .next()
                            .unwrap_or(&s)
                            .trim()
                            .rsplit(' ')
                            .next()
                            .unwrap_or(&s)
                            .trim_start_matches('v')
                            .to_string(),
                    )
                }
            })
    } else {
        None
    };
    (installed, version)
}
