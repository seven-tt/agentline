use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Embedded copy of the repo's `config.example.toml`. Materialized to
/// disk on first run by [`AppConfig::ensure_exists`].
pub const DEFAULT_CONFIG_TEMPLATE: &str = include_str!("../../../config.example.toml");

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AppConfig {
    #[serde(default)]
    pub im: ImSection,
    #[serde(default)]
    pub agent: AgentSection,
    #[serde(default)]
    pub bridge: BridgeSection,
    #[serde(default)]
    pub web: WebSection,
    #[serde(default)]
    pub proxy: ProxySection,
    #[serde(default)]
    pub log: LogSection,
    #[serde(default)]
    pub transport: TransportSection,
    #[serde(default)]
    pub projects: Vec<ProjectConfig>,
    /// Path to the TOML config file (set after deserialization).
    #[serde(skip)]
    pub config_path: Option<PathBuf>,
}

/// Logging configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct LogSection {
    /// Base log level for agentline: `error` | `warn` | `info` | `debug` |
    /// `trace`. Most operational detail is at `debug`; `info` (default) shows
    /// the message flow and key milestones; errors always print. Noisy
    /// transitive deps (hyper/reqwest/rustls/…) are pinned to `warn`
    /// regardless. The `RUST_LOG` env var, if set, overrides this entirely.
    #[serde(default = "default_log_level")]
    pub level: String,
}

impl Default for LogSection {
    fn default() -> Self {
        Self {
            level: default_log_level(),
        }
    }
}

fn default_log_level() -> String {
    "info".to_string()
}

/// A project the agent can be directed to clone and develop.
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectConfig {
    /// Short identifier shown in `/project` listings (e.g. "agentline").
    pub name: String,
    /// Git remote URL, e.g. "https://github.com/org/repo.git".
    pub git_url: String,
}

/// Proxy settings injected into every agent subprocess.
///
/// All fields are optional — leave empty to inherit the parent process's
/// environment. RFC-1918 LAN ranges are always added to `NO_PROXY`
/// regardless of these settings.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProxySection {
    /// HTTP proxy URL, e.g. `"http://proxy.example.com:8080"`.
    /// Sets `HTTP_PROXY` and `http_proxy` on the child process.
    /// Empty = pass through the parent process's `HTTP_PROXY`.
    #[serde(default)]
    pub http: String,
    /// HTTPS proxy URL. Sets `HTTPS_PROXY` and `https_proxy`.
    /// Empty = fall back to the `http` setting, then parent's `HTTPS_PROXY`.
    #[serde(default)]
    pub https: String,
    /// Extra `no_proxy` entries (comma-separated).
    /// Merged with the parent's `NO_PROXY` and the built-in LAN exclusions.
    /// Example: `"myhost.internal,.corp.example.com"`
    #[serde(default)]
    pub no_proxy: String,
}

/// Transport layer: unix socket and iroh P2P.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct TransportSection {
    /// Shared secret for connection authentication + message signing.
    /// Empty = no auth (local development only).
    #[serde(default)]
    pub token: String,
    /// Path for the unix domain socket. Empty = disabled.
    #[serde(default)]
    pub unix_socket: String,
    #[serde(default)]
    #[cfg_attr(not(feature = "iroh"), allow(dead_code))]
    pub iroh: IrohTransportCfg,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[cfg_attr(not(feature = "iroh"), allow(dead_code))]
pub struct IrohTransportCfg {
    #[serde(default)]
    pub enable: bool,
    /// Hex-encoded 32-byte secret key for a stable NodeId across restarts.
    /// If empty, auto-generated and persisted to `<state_dir>/iroh.key`.
    #[serde(default)]
    pub secret_key: String,
    /// Custom relay server URL. Empty = use default n0 relays.
    #[serde(default)]
    pub relay_url: String,
}

/// Embedded dashboard HTTP server.
#[derive(Debug, Clone, Deserialize)]
pub struct WebSection {
    /// Set to false to skip starting the dashboard. Default: true.
    #[serde(default = "default_true")]
    pub enable: bool,
    /// Bind address. Default: `127.0.0.1:7681` (localhost only).
    #[serde(default = "default_web_bind")]
    pub bind: String,
}

impl Default for WebSection {
    fn default() -> Self {
        Self {
            enable: true,
            bind: default_web_bind(),
        }
    }
}

fn default_web_bind() -> String {
    "127.0.0.1:7681".into()
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ImSection {
    #[serde(default)]
    pub wechat: WechatBackendCfg,
    #[serde(default)]
    pub dingtalk: DingtalkBackendCfg,
    #[serde(default)]
    pub feishu: FeishuBackendCfg,
    #[serde(default)]
    pub telegram: TelegramBackendCfg,
}

impl ImSection {
    pub fn enabled_backends(&self) -> Vec<&str> {
        let mut v = Vec::new();
        if self.wechat.enable {
            v.push("wechat");
        }
        if self.dingtalk.enable {
            v.push("dingtalk");
        }
        if self.feishu.enable {
            v.push("feishu");
        }
        if self.telegram.enable {
            v.push("telegram");
        }
        v
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct WechatBackendCfg {
    #[serde(default)]
    pub enable: bool,
    #[serde(default)]
    pub allowed_users: Vec<String>,
    #[serde(default = "default_typing_interval")]
    pub typing_interval_ms: u64,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DingtalkBackendCfg {
    #[serde(default)]
    pub enable: bool,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default)]
    pub allowed_users: Vec<String>,
    #[serde(default)]
    pub card_template_id: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FeishuBackendCfg {
    #[serde(default)]
    pub enable: bool,
    #[serde(default)]
    pub app_id: String,
    #[serde(default)]
    pub app_secret: String,
    #[serde(default)]
    pub allowed_users: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct TelegramBackendCfg {
    #[serde(default)]
    pub enable: bool,
    #[serde(default)]
    pub bot_token: String,
    #[serde(default)]
    pub allowed_users: Vec<String>,
    #[serde(default)]
    pub api_base: String,
}

fn default_typing_interval() -> u64 {
    5000
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentSection {
    /// "claude-code" | "kimi" | "qoder" | "opencode" | "kiro" | "gemini" | "hermes" | "codex" | "acp"
    #[serde(default = "default_agent_backend")]
    pub backend: String,
    #[serde(default, rename = "claude-code")]
    pub claude_code: ClaudeCodeBackendCfg,
    #[serde(default)]
    pub kimi: KimiBackendCfg,
    #[serde(default)]
    pub qoder: QoderBackendCfg,
    #[serde(default)]
    pub opencode: OpencodeBackendCfg,
    #[serde(default)]
    pub kiro: KiroBackendCfg,
    #[serde(default)]
    pub gemini: GeminiBackendCfg,
    #[serde(default)]
    pub hermes: HermesBackendCfg,
    #[serde(default)]
    pub codex: CodexBackendCfg,
    #[serde(default)]
    pub acp: AcpBackendCfg,
}

impl Default for AgentSection {
    fn default() -> Self {
        Self {
            backend: default_agent_backend(),
            claude_code: ClaudeCodeBackendCfg::default(),
            kimi: KimiBackendCfg::default(),
            qoder: QoderBackendCfg::default(),
            opencode: OpencodeBackendCfg::default(),
            kiro: KiroBackendCfg::default(),
            gemini: GeminiBackendCfg::default(),
            hermes: HermesBackendCfg::default(),
            codex: CodexBackendCfg::default(),
            acp: AcpBackendCfg::default(),
        }
    }
}

fn default_agent_backend() -> String {
    "claude-code".into()
}

/// Maps to `agentline_agent_gemini::GeminiConfig`.
#[derive(Debug, Clone, Deserialize, Default, Serialize)]
pub struct GeminiBackendCfg {
    /// Override launcher (default: `gemini`). Empty = use default.
    #[serde(default)]
    pub command: String,
    /// Override launcher args (default: `["--acp"]`). Empty = use default.
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub extra_env: Vec<(String, String)>,
    #[serde(default)]
    pub remove_env: Vec<String>,
}

/// Maps to `agentline_agent_kiro::KiroConfig`.
#[derive(Debug, Clone, Deserialize, Default, Serialize)]
pub struct KiroBackendCfg {
    /// Override launcher (default: `kiro-cli`). Pass absolute path if not on
    /// the daemon's PATH (e.g. `~/.local/bin/kiro-cli`).
    #[serde(default)]
    pub command: String,
    /// Override launcher args entirely. Empty = use default `["acp"]`
    /// (plus `["--agent", <name>]` if `agent_name` is non-empty).
    #[serde(default)]
    pub args: Vec<String>,
    /// Use a specific Kiro agent configuration (`--agent <name>`).
    #[serde(default)]
    pub agent_name: String,
    #[serde(default)]
    pub extra_env: Vec<(String, String)>,
    #[serde(default)]
    pub remove_env: Vec<String>,
}

/// Maps to `agentline_agent_opencode::OpencodeConfig`.
#[derive(Debug, Clone, Deserialize, Default, Serialize)]
pub struct OpencodeBackendCfg {
    /// Override launcher (default: `opencode`). Empty = use default.
    #[serde(default)]
    pub command: String,
    /// Override launcher args (default: `["acp"]`). Empty = use default.
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub extra_env: Vec<(String, String)>,
    #[serde(default)]
    pub remove_env: Vec<String>,
}

/// Maps to `agentline_agent_qoder::QoderConfig`.
#[derive(Debug, Clone, Deserialize, Default, Serialize)]
pub struct QoderBackendCfg {
    /// Override launcher (default: `qodercli`). Empty = use default.
    #[serde(default)]
    pub command: String,
    /// Override launcher args (default: `["--acp"]`). Empty = use default.
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub extra_env: Vec<(String, String)>,
    #[serde(default)]
    pub remove_env: Vec<String>,
}

/// Maps to `agentline_agent_hermes::HermesConfig`.
#[derive(Debug, Clone, Deserialize, Default, Serialize)]
pub struct HermesBackendCfg {
    /// Override launcher (default: `hermes`). Empty = use default.
    #[serde(default)]
    pub command: String,
    /// Override launcher args (default: `["acp"]`). Empty = use default.
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub extra_env: Vec<(String, String)>,
    #[serde(default)]
    pub remove_env: Vec<String>,
}

/// Maps to `agentline_agent_codex::CodexConfig`.
#[derive(Debug, Clone, Deserialize, Default, Serialize)]
pub struct CodexBackendCfg {
    /// Override the codex binary (default: `codex` on PATH). Empty = default.
    #[serde(default)]
    pub command: String,
    /// Override args (default: `["app-server"]`). Empty = default.
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub extra_env: Vec<(String, String)>,
    /// "read-only" | "workspace-write" | "danger-full-access". Default: workspace-write.
    #[serde(default)]
    pub sandbox_mode: String,
    /// "never" | "on-request" | "on-failure" | "untrusted". Default: never.
    #[serde(default)]
    pub approval_mode: String,
    /// Don't refuse to run outside a git repo. Default: true.
    #[serde(default = "default_true")]
    pub skip_git_repo_check: bool,
    /// Override codex's default model (e.g. `"gpt-5-codex"`). Empty = use codex default.
    #[serde(default)]
    pub model: String,
}

fn default_true() -> bool {
    true
}

/// Maps to `agentline_agent_kimi::KimiConfig`.
#[derive(Debug, Clone, Deserialize, Default, Serialize)]
pub struct KimiBackendCfg {
    /// Override launcher (default: `kimi`). Empty = use default.
    #[serde(default)]
    pub command: String,
    /// Override launcher args (default: `["acp"]`). Empty = use default.
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub extra_env: Vec<(String, String)>,
    #[serde(default)]
    pub remove_env: Vec<String>,
}

/// Maps to `agentline_agent_claude_code::ClaudeCodeConfig`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClaudeCodeBackendCfg {
    /// npm version tag of `@zed-industries/claude-code-acp`. Empty = crate default.
    #[serde(default)]
    pub version: String,
    /// Override launcher (default: `npx`). Empty = use default.
    #[serde(default)]
    pub command: String,
    /// Override launcher args entirely. Empty = use default `-y @zed-industries/claude-code-acp@<version>`.
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra env vars set on the child process.
    #[serde(default)]
    pub extra_env: Vec<(String, String)>,
    /// Additional env vars to strip from the child. The crate's hard-coded
    /// defaults (CLAUDECODE family + ANTHROPIC_API_KEY) are always stripped.
    #[serde(default)]
    pub remove_env_extra: Vec<String>,
    /// Read `~/.claude/settings.json` and apply its `env` block. Default: true.
    #[serde(default = "default_inject_settings")]
    pub inject_settings_env: bool,
}

impl Default for ClaudeCodeBackendCfg {
    fn default() -> Self {
        Self {
            version: String::new(),
            command: String::new(),
            args: Vec::new(),
            extra_env: Vec::new(),
            remove_env_extra: Vec::new(),
            inject_settings_env: true,
        }
    }
}

fn default_inject_settings() -> bool {
    true
}

/// Maps to `agentline_agent_acp::AcpBackendConfig`. Used only when
/// `agent.backend = "acp"` (generic / advanced).
#[derive(Debug, Clone, Deserialize, Default, Serialize)]
pub struct AcpBackendCfg {
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub extra_env: Vec<(String, String)>,
    #[serde(default)]
    pub remove_env: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BridgeSection {
    #[serde(default = "default_cwd")]
    pub default_cwd: String,
    #[serde(default = "default_state_dir")]
    pub state_dir: String,
    /// Close and recreate the agent session after this many seconds of
    /// inactivity. `0` disables the timeout (session lives until `/new` or
    /// process exit). Default: 7200 (2 hours).
    #[serde(default = "default_session_idle_timeout_secs")]
    pub session_idle_timeout_secs: u64,
    /// UI language: "zh-CN" | "en". Default: "zh-CN".
    #[serde(default = "default_locale")]
    pub locale: String,
}

impl Default for BridgeSection {
    fn default() -> Self {
        Self {
            default_cwd: default_cwd(),
            state_dir: default_state_dir(),
            session_idle_timeout_secs: default_session_idle_timeout_secs(),
            locale: default_locale(),
        }
    }
}

fn default_locale() -> String {
    "zh-CN".into()
}

fn default_session_idle_timeout_secs() -> u64 {
    7200 // 2 hours
}

fn default_cwd() -> String {
    // Empty = auto: bridge uses `<state_dir>/agents/<agent_backend>`,
    // isolating each agent in its own workspace instead of $HOME.
    String::new()
}
fn default_state_dir() -> String {
    "~/.agentline".into()
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read config {}", path.display()))?;
        let cfg: Self =
            toml::from_str(&text).with_context(|| format!("parse config {}", path.display()))?;
        Ok(cfg)
    }

    /// If `path` doesn't exist, write the embedded
    /// [`DEFAULT_CONFIG_TEMPLATE`] there (creating parent dirs as needed)
    /// and return `Ok(true)`. Returns `Ok(false)` if the file already
    /// existed.
    pub fn ensure_exists(path: &Path) -> Result<bool> {
        if path.exists() {
            return Ok(false);
        }
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir {}", parent.display()))?;
        }
        std::fs::write(path, DEFAULT_CONFIG_TEMPLATE)
            .with_context(|| format!("write default config to {}", path.display()))?;
        Ok(true)
    }

    /// Convenience: ensure the file exists (writing the template if not),
    /// then parse it. Returns the loaded config plus whether the file was
    /// freshly created on this call.
    pub fn load_or_init(path: &Path) -> Result<(Self, bool)> {
        let created = Self::ensure_exists(path)?;
        let cfg = Self::load(path)?;
        Ok((cfg, created))
    }

    pub fn state_path(&self) -> Result<PathBuf> {
        Ok(expand_tilde(&self.bridge.state_dir).join("state.json"))
    }

    pub fn resolved_cwd(&self) -> PathBuf {
        expand_tilde(&self.bridge.default_cwd)
    }
}

pub fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    if p == "~"
        && let Some(home) = dirs::home_dir()
    {
        return home;
    }
    PathBuf::from(p)
}

pub fn default_config_path() -> PathBuf {
    expand_tilde("~/.agentline/config.toml")
}
