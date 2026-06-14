use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use agentline_bridge::{Bridge, SessionRegistry};
use agentline_im_wechat::{HttpClient, request_qr, wait_for_scan};
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::config::AppConfig;
use crate::state::AppState;

const DASHBOARD_HTML: &str = include_str!("../templates/dashboard.html");

#[derive(Clone)]
pub struct Web {
    cfg: Arc<AppConfig>,
    login: Arc<Mutex<LoginState>>,
    bridge_trigger: Arc<tokio::sync::Notify>,
    registry: Arc<SessionRegistry>,
    bridge: Arc<std::sync::OnceLock<Bridge>>,
    start_time: Instant,
}

#[derive(Default)]
struct LoginState {
    task: Option<tokio::task::JoinHandle<()>>,
    state: String,
    message: String,
    qr_login_url: Option<String>,
}

pub fn start(
    cfg: Arc<AppConfig>,
    bind: &str,
    bridge_trigger: Arc<tokio::sync::Notify>,
    registry: Arc<SessionRegistry>,
) -> (
    tokio::task::JoinHandle<()>,
    Arc<std::sync::OnceLock<Bridge>>,
) {
    let bind = bind.to_string();
    let bridge_holder: Arc<std::sync::OnceLock<Bridge>> = Arc::new(std::sync::OnceLock::new());
    let app_state = Web {
        cfg,
        login: Arc::new(Mutex::new(LoginState::default())),
        bridge_trigger,
        registry,
        bridge: bridge_holder.clone(),
        start_time: Instant::now(),
    };

    let router = Router::new()
        .route("/", get(serve_index))
        // Overview
        .route("/api/overview", get(api_overview))
        // Channels (IM)
        .route(
            "/api/channels",
            get(api_channels_get).post(api_channels_set),
        )
        .route("/api/channels/wechat/login/start", post(api_login_start))
        .route("/api/channels/wechat/login/cancel", post(api_login_cancel))
        .route("/api/channels/wechat/login/status", get(api_login_status))
        .route("/api/channels/wechat/login/qr.png", get(api_login_qr))
        // Agents
        .route("/api/agents", get(api_agents))
        .route("/api/agents/config", post(api_agents_config))
        .route("/api/agents/{id}/install", post(api_agents_install))
        .route(
            "/api/agents/{id}/check-update",
            post(api_agents_check_update),
        )
        // Projects
        .route("/api/projects", get(api_projects_get).put(api_projects_set))
        // Settings
        .route(
            "/api/settings",
            get(api_settings_get).post(api_settings_set),
        )
        .route("/api/settings/restart", post(api_restart))
        // Logs
        .route("/api/logs", get(api_logs))
        .with_state(app_state);

    let handle = tokio::spawn(async move {
        let listener = match tokio::net::TcpListener::bind(&bind).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(error=%e, bind=%bind, "web: bind failed; dashboard disabled");
                return;
            }
        };
        tracing::info!(addr = %bind, "web dashboard listening (http://{bind})");
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!(error=%e, "web server stopped");
        }
    });
    (handle, bridge_holder)
}

async fn serve_index() -> Html<&'static str> {
    Html(DASHBOARD_HTML)
}

fn config_path(w: &Web) -> PathBuf {
    w.cfg
        .config_path
        .clone()
        .unwrap_or_else(crate::config::default_config_path)
}

fn reload_cfg(w: &Web) -> AppConfig {
    let path = config_path(w);
    AppConfig::load(&path).unwrap_or_else(|_| (*w.cfg).clone())
}

// ─── /api/overview ─────────────────────────────────────────────

#[derive(Serialize)]
struct OverviewOut {
    version: &'static str,
    uptime_secs: u64,
    pid: u32,
    agent_backend: String,
    ims: Vec<ImStatusOut>,
}

#[derive(Serialize)]
struct ImStatusOut {
    id: String,
    enabled: bool,
    healthy: bool,
    sessions: Vec<SessionOut>,
}

#[derive(Serialize)]
struct SessionOut {
    id: String,
    user: String,
    active: bool,
    cwd: String,
}

async fn api_overview(State(w): State<Web>) -> axum::Json<OverviewOut> {
    let cfg = reload_cfg(&w);
    let snap = w.registry.snapshot();
    let im_ids = ["wechat", "dingtalk", "feishu", "telegram"];
    let ims = im_ids
        .iter()
        .map(|&id| {
            let enabled = match id {
                "wechat" => cfg.im.wechat.enable,
                "dingtalk" => cfg.im.dingtalk.enable,
                "feishu" => cfg.im.feishu.enable,
                "telegram" => cfg.im.telegram.enable,
                _ => false,
            };
            let (healthy, sessions) = match snap.get(id) {
                Some(s) => (
                    s.healthy,
                    s.sessions
                        .iter()
                        .map(|ss| SessionOut {
                            id: ss.id.clone(),
                            user: ss.user.clone(),
                            active: ss.active,
                            cwd: ss.cwd.clone(),
                        })
                        .collect(),
                ),
                None => (false, vec![]),
            };
            ImStatusOut {
                id: id.to_string(),
                enabled,
                healthy,
                sessions,
            }
        })
        .collect();

    axum::Json(OverviewOut {
        version: env!("CARGO_PKG_VERSION"),
        uptime_secs: w.start_time.elapsed().as_secs(),
        pid: std::process::id(),
        agent_backend: cfg.agent.backend.clone(),
        ims,
    })
}

// ─── /api/channels ─────────────────────────────────────────────

#[derive(Serialize)]
struct ChannelsOut {
    wechat: WechatChannelOut,
    dingtalk: DingtalkChannelOut,
    feishu: FeishuChannelOut,
    telegram: TelegramChannelOut,
}

#[derive(Serialize)]
struct WechatChannelOut {
    enable: bool,
    allowed_users: Vec<String>,
    typing_interval_ms: u64,
    logged_in: bool,
}

#[derive(Serialize)]
struct DingtalkChannelOut {
    enable: bool,
    client_id: String,
    client_secret: String,
    allowed_users: Vec<String>,
}

#[derive(Serialize)]
struct FeishuChannelOut {
    enable: bool,
    app_id: String,
    app_secret: String,
    allowed_users: Vec<String>,
}

#[derive(Serialize)]
struct TelegramChannelOut {
    enable: bool,
    bot_token: String,
    api_base: String,
    allowed_users: Vec<String>,
}

async fn api_channels_get(State(w): State<Web>) -> axum::Json<ChannelsOut> {
    let cfg = reload_cfg(&w);
    let im = &cfg.im;
    let wechat_logged_in = w
        .cfg
        .state_path()
        .ok()
        .and_then(|p| AppState::load_or_default(&p).ok())
        .and_then(|s| s.im.wechat.bot_token)
        .is_some();
    axum::Json(ChannelsOut {
        wechat: WechatChannelOut {
            enable: im.wechat.enable,
            allowed_users: im.wechat.allowed_users.clone(),
            typing_interval_ms: im.wechat.typing_interval_ms,
            logged_in: wechat_logged_in,
        },
        dingtalk: DingtalkChannelOut {
            enable: im.dingtalk.enable,
            client_id: im.dingtalk.client_id.clone(),
            client_secret: im.dingtalk.client_secret.clone(),
            allowed_users: im.dingtalk.allowed_users.clone(),
        },
        feishu: FeishuChannelOut {
            enable: im.feishu.enable,
            app_id: im.feishu.app_id.clone(),
            app_secret: im.feishu.app_secret.clone(),
            allowed_users: im.feishu.allowed_users.clone(),
        },
        telegram: TelegramChannelOut {
            enable: im.telegram.enable,
            bot_token: im.telegram.bot_token.clone(),
            api_base: im.telegram.api_base.clone(),
            allowed_users: im.telegram.allowed_users.clone(),
        },
    })
}

#[derive(Deserialize, Default)]
struct ChannelsIn {
    #[serde(default)]
    wechat: Option<WechatChannelIn>,
    #[serde(default)]
    dingtalk: Option<DingtalkChannelIn>,
    #[serde(default)]
    feishu: Option<FeishuChannelIn>,
    #[serde(default)]
    telegram: Option<TelegramChannelIn>,
}

#[derive(Deserialize)]
struct WechatChannelIn {
    enable: Option<bool>,
    allowed_users: Option<Vec<String>>,
    typing_interval_ms: Option<u64>,
}

#[derive(Deserialize)]
struct DingtalkChannelIn {
    enable: Option<bool>,
    client_id: Option<String>,
    client_secret: Option<String>,
    allowed_users: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct FeishuChannelIn {
    enable: Option<bool>,
    app_id: Option<String>,
    app_secret: Option<String>,
    allowed_users: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct TelegramChannelIn {
    enable: Option<bool>,
    bot_token: Option<String>,
    api_base: Option<String>,
    allowed_users: Option<Vec<String>>,
}

async fn api_channels_set(
    State(w): State<Web>,
    axum::Json(body): axum::Json<ChannelsIn>,
) -> Response {
    let path = config_path(&w);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => return err_response(format!("read config: {e}")),
    };

    let mut updated = text;

    // Write per-IM enable flags
    if let Some(ref wc) = body.wechat
        && let Some(v) = wc.enable
    {
        updated = set_toml_bool(&updated, "im.wechat", "enable", v);
    }
    if let Some(ref dt) = body.dingtalk
        && let Some(v) = dt.enable
    {
        updated = set_toml_bool(&updated, "im.dingtalk", "enable", v);
    }
    if let Some(ref fs) = body.feishu
        && let Some(v) = fs.enable
    {
        updated = set_toml_bool(&updated, "im.feishu", "enable", v);
    }
    if let Some(ref tg) = body.telegram
        && let Some(v) = tg.enable
    {
        updated = set_toml_bool(&updated, "im.telegram", "enable", v);
    }

    if let Some(ref wc) = body.wechat {
        if let Some(ref v) = wc.allowed_users {
            updated = set_toml_string_array(&updated, "im.wechat", "allowed_users", v);
        }
        if let Some(v) = wc.typing_interval_ms {
            updated = set_toml_int(&updated, "im.wechat", "typing_interval_ms", v);
        }
    }
    if let Some(ref dt) = body.dingtalk {
        if let Some(ref v) = dt.client_id {
            updated = set_toml_key(&updated, "im.dingtalk", "client_id", v);
        }
        if let Some(ref v) = dt.client_secret {
            updated = set_toml_key(&updated, "im.dingtalk", "client_secret", v);
        }
        if let Some(ref v) = dt.allowed_users {
            updated = set_toml_string_array(&updated, "im.dingtalk", "allowed_users", v);
        }
    }
    if let Some(ref fs) = body.feishu {
        if let Some(ref v) = fs.app_id {
            updated = set_toml_key(&updated, "im.feishu", "app_id", v);
        }
        if let Some(ref v) = fs.app_secret {
            updated = set_toml_key(&updated, "im.feishu", "app_secret", v);
        }
        if let Some(ref v) = fs.allowed_users {
            updated = set_toml_string_array(&updated, "im.feishu", "allowed_users", v);
        }
    }
    if let Some(ref tg) = body.telegram {
        if let Some(ref v) = tg.bot_token {
            updated = set_toml_key(&updated, "im.telegram", "bot_token", v);
        }
        if let Some(ref v) = tg.api_base {
            updated = set_toml_key(&updated, "im.telegram", "api_base", v);
        }
        if let Some(ref v) = tg.allowed_users {
            updated = set_toml_string_array(&updated, "im.telegram", "allowed_users", v);
        }
    }

    if let Err(e) = std::fs::write(&path, &updated) {
        return err_response(format!("write config: {e}"));
    }
    ok_response("saved")
}

// ─── /api/channels/wechat/login/* ──────────────────────────────

#[derive(Serialize)]
struct LoginStatusOut {
    state: String,
    message: String,
}

async fn api_login_start(State(w): State<Web>) -> Response {
    let mut login = w.login.lock().await;

    if let Some(t) = login.task.as_ref()
        && t.is_finished()
    {
        login.task = None;
    }
    if login.task.is_some() {
        return (StatusCode::CONFLICT, "login already in progress").into_response();
    }

    login.state = "starting".into();
    login.message.clear();
    login.qr_login_url = None;

    let state_path = match w.cfg.state_path() {
        Ok(p) => p,
        Err(e) => {
            login.state = "failed".into();
            login.message = format!("state path: {e}");
            return err_response(login.message.clone());
        }
    };

    let login_arc = w.login.clone();
    let trigger = w.bridge_trigger.clone();
    let handle = tokio::spawn(async move {
        run_login(login_arc, state_path, trigger).await;
    });
    login.task = Some(handle);

    (StatusCode::ACCEPTED, "login started").into_response()
}

async fn api_login_cancel(State(w): State<Web>) -> Response {
    let mut login = w.login.lock().await;
    if let Some(task) = login.task.take() {
        task.abort();
    }
    login.state = "idle".into();
    login.message.clear();
    login.qr_login_url = None;
    ok_response("cancelled")
}

async fn api_login_status(State(w): State<Web>) -> axum::Json<LoginStatusOut> {
    let mut login = w.login.lock().await;
    if let Some(t) = login.task.as_ref()
        && t.is_finished()
    {
        login.task = None;
    }
    if login.state.is_empty() {
        login.state = "idle".into();
    }
    axum::Json(LoginStatusOut {
        state: login.state.clone(),
        message: login.message.clone(),
    })
}

async fn api_login_qr(State(w): State<Web>) -> Response {
    let login = w.login.lock().await;
    match login.qr_login_url.clone() {
        Some(url) => {
            let svg = qrcode::QrCode::new(url.as_bytes())
                .map(|c| {
                    c.render::<qrcode::render::svg::Color<'_>>()
                        .min_dimensions(240, 240)
                        .build()
                })
                .unwrap_or_default();
            (
                [
                    (header::CONTENT_TYPE, "image/svg+xml"),
                    (header::CACHE_CONTROL, "no-store"),
                ],
                svg,
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "no QR yet").into_response(),
    }
}

// ─── /api/agents ───────────────────────────────────────────────

#[derive(Serialize)]
struct AgentsOut {
    backend: String,
    platform: String,
    list: Vec<AgentItem>,
    configs: AgentConfigs,
}

#[derive(Serialize)]
struct AgentItem {
    id: String,
    installed: bool,
    version: Option<String>,
    status: String,
}

#[derive(Serialize)]
struct AgentConfigs {
    codex: CodexConfigOut,
    qoder: QoderConfigOut,
    opencode: OpencodeConfigOut,
    kimi: KimiConfigOut,
    gemini: GeminiConfigOut,
}

#[derive(Serialize)]
struct CodexConfigOut {
    model: String,
    sandbox_mode: String,
    approval_mode: String,
    api_key: String,
}

#[derive(Serialize)]
struct QoderConfigOut {
    personal_access_token: String,
}

#[derive(Serialize)]
struct OpencodeConfigOut {
    base_url: String,
    api_key: String,
}

#[derive(Serialize)]
struct KimiConfigOut {
    access_token: String,
}

#[derive(Serialize)]
struct GeminiConfigOut {
    api_key: String,
}

use crate::agents::{AGENTS, detect_agent};

async fn api_agents(State(w): State<Web>) -> axum::Json<AgentsOut> {
    let cfg = reload_cfg(&w);
    let list: Vec<AgentItem> = AGENTS
        .iter()
        .map(|meta| {
            let (installed, version) = detect_agent(meta.cli_cmd);
            AgentItem {
                id: meta.id.to_string(),
                installed,
                version,
                status: meta.status(installed, &cfg.agent).to_string(),
            }
        })
        .collect();

    axum::Json(AgentsOut {
        backend: cfg.agent.backend.clone(),
        platform: std::env::consts::OS.to_string(),
        list,
        configs: AgentConfigs {
            codex: CodexConfigOut {
                model: cfg.agent.codex.model.clone(),
                sandbox_mode: if cfg.agent.codex.sandbox_mode.is_empty() {
                    "workspace-write".to_string()
                } else {
                    cfg.agent.codex.sandbox_mode.clone()
                },
                approval_mode: if cfg.agent.codex.approval_mode.is_empty() {
                    "never".to_string()
                } else {
                    cfg.agent.codex.approval_mode.clone()
                },
                api_key: cfg.agent.codex.api_key.clone(),
            },
            qoder: QoderConfigOut {
                personal_access_token: cfg.agent.qoder.personal_access_token.clone(),
            },
            opencode: OpencodeConfigOut {
                base_url: cfg.agent.opencode.base_url.clone(),
                api_key: cfg.agent.opencode.api_key.clone(),
            },
            kimi: KimiConfigOut {
                access_token: cfg.agent.kimi.access_token.clone(),
            },
            gemini: GeminiConfigOut {
                api_key: cfg.agent.gemini.api_key.clone(),
            },
        },
    })
}

#[derive(Deserialize)]
struct AgentConfigIn {
    backend: Option<String>,
    codex: Option<CodexConfigIn>,
    qoder: Option<QoderConfigIn>,
    opencode: Option<OpencodeConfigIn>,
    kimi: Option<KimiConfigIn>,
    gemini: Option<GeminiConfigIn>,
}

#[derive(Deserialize)]
struct CodexConfigIn {
    model: Option<String>,
    sandbox_mode: Option<String>,
    approval_mode: Option<String>,
    api_key: Option<String>,
}

#[derive(Deserialize)]
struct QoderConfigIn {
    personal_access_token: Option<String>,
}

#[derive(Deserialize)]
struct OpencodeConfigIn {
    base_url: Option<String>,
    api_key: Option<String>,
}

#[derive(Deserialize)]
struct KimiConfigIn {
    access_token: Option<String>,
}

#[derive(Deserialize)]
struct GeminiConfigIn {
    api_key: Option<String>,
}

async fn api_agents_config(
    State(w): State<Web>,
    axum::Json(body): axum::Json<AgentConfigIn>,
) -> Response {
    let path = config_path(&w);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => return err_response(format!("read config: {e}")),
    };

    let mut updated = text;
    if let Some(ref b) = body.backend {
        updated = set_toml_backend(&updated, "agent", b);
    }
    if let Some(ref c) = body.codex {
        if let Some(ref v) = c.model {
            updated = set_toml_key(&updated, "agent.codex", "model", v);
        }
        if let Some(ref v) = c.sandbox_mode {
            updated = set_toml_key(&updated, "agent.codex", "sandbox_mode", v);
        }
        if let Some(ref v) = c.approval_mode {
            updated = set_toml_key(&updated, "agent.codex", "approval_mode", v);
        }
        if let Some(ref v) = c.api_key {
            updated = set_toml_key(&updated, "agent.codex", "api_key", v);
        }
    }
    if let Some(ref q) = body.qoder
        && let Some(ref v) = q.personal_access_token
    {
        updated = set_toml_key(&updated, "agent.qoder", "personal_access_token", v);
    }
    if let Some(ref o) = body.opencode {
        if let Some(ref v) = o.base_url {
            updated = set_toml_key(&updated, "agent.opencode", "base_url", v);
        }
        if let Some(ref v) = o.api_key {
            updated = set_toml_key(&updated, "agent.opencode", "api_key", v);
        }
    }
    if let Some(ref k) = body.kimi
        && let Some(ref v) = k.access_token
    {
        updated = set_toml_key(&updated, "agent.kimi", "access_token", v);
    }
    if let Some(ref g) = body.gemini
        && let Some(ref v) = g.api_key
    {
        updated = set_toml_key(&updated, "agent.gemini", "api_key", v);
    }

    if let Err(e) = std::fs::write(&path, &updated) {
        return err_response(format!("write config: {e}"));
    }
    ok_response("saved")
}

// ─── /api/agents/:id/install & check-update ────────────────────

#[derive(Serialize)]
struct InstallResult {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

async fn api_agents_install(
    Path(id): Path<String>,
    State(w): State<Web>,
) -> axum::Json<InstallResult> {
    let meta = AGENTS.iter().find(|m| m.id == id.as_str());
    let args = meta.and_then(|m| m.install);
    let Some(args) = args else {
        return axum::Json(InstallResult {
            success: false,
            error: Some(format!("unknown agent: {id}")),
        });
    };

    // sh-based installers require Unix
    if cfg!(target_os = "windows") && args[0] == "sh" {
        return axum::Json(InstallResult {
            success: false,
            error: Some(format!("{id} does not support Windows installation")),
        });
    }

    let cfg = reload_cfg(&w);
    let proxy_http = cfg.proxy.http.clone();
    let proxy_https = cfg.proxy.https.clone();
    let proxy_no = cfg.proxy.no_proxy.clone();

    tracing::info!(agent = %id, cmd = ?args, "installing agent");

    let result = tokio::task::spawn_blocking({
        let args = args.to_vec();
        move || {
            let mut cmd = std::process::Command::new(args[0]);
            cmd.args(&args[1..]);
            if !proxy_http.is_empty() {
                cmd.env("HTTP_PROXY", &proxy_http);
                cmd.env("http_proxy", &proxy_http);
            }
            if !proxy_https.is_empty() {
                cmd.env("HTTPS_PROXY", &proxy_https);
                cmd.env("https_proxy", &proxy_https);
            } else if !proxy_http.is_empty() {
                cmd.env("HTTPS_PROXY", &proxy_http);
                cmd.env("https_proxy", &proxy_http);
            }
            if !proxy_no.is_empty() {
                cmd.env("NO_PROXY", &proxy_no);
                cmd.env("no_proxy", &proxy_no);
            }
            cmd.output()
        }
    })
    .await;

    match result {
        Ok(Ok(output)) if output.status.success() => {
            tracing::info!(agent = %id, "agent installed successfully");
            axum::Json(InstallResult {
                success: true,
                error: None,
            })
        }
        Ok(Ok(output)) => {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let detail = if stderr.is_empty() { stdout } else { stderr };
            tracing::error!(agent = %id, exit = ?output.status, detail = %detail, "agent install failed");
            axum::Json(InstallResult {
                success: false,
                error: Some(detail),
            })
        }
        Ok(Err(e)) => {
            tracing::error!(agent = %id, error = %e, "agent install command failed to execute");
            axum::Json(InstallResult {
                success: false,
                error: Some(format!("{}: {e}", args[0])),
            })
        }
        Err(e) => {
            tracing::error!(agent = %id, error = %e, "agent install task panicked");
            axum::Json(InstallResult {
                success: false,
                error: Some(e.to_string()),
            })
        }
    }
}

#[derive(Serialize)]
struct CheckUpdateResult {
    current: Option<String>,
    latest: Option<String>,
    has_update: bool,
}

async fn api_agents_check_update(Path(id): Path<String>) -> axum::Json<CheckUpdateResult> {
    let cmd = AGENTS
        .iter()
        .find(|m| m.id == id.as_str())
        .map(|m| m.cli_cmd)
        .unwrap_or("");

    let (installed, current) = detect_agent(cmd);
    if !installed {
        return axum::Json(CheckUpdateResult {
            current: None,
            latest: None,
            has_update: false,
        });
    }

    axum::Json(CheckUpdateResult {
        current: current.clone(),
        latest: current,
        has_update: false,
    })
}

// ─── /api/projects ─────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
struct ProjectItem {
    name: String,
    git_url: String,
}

async fn api_projects_get(State(w): State<Web>) -> axum::Json<Vec<ProjectItem>> {
    let cfg = reload_cfg(&w);
    let items = cfg
        .projects
        .iter()
        .map(|p| ProjectItem {
            name: p.name.clone(),
            git_url: p.git_url.clone(),
        })
        .collect();
    axum::Json(items)
}

async fn api_projects_set(
    State(w): State<Web>,
    axum::Json(projects): axum::Json<Vec<ProjectItem>>,
) -> Response {
    let path = config_path(&w);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => return err_response(format!("read: {e}")),
    };

    let updated = set_projects_toml(&text, &projects);
    if let Err(e) = std::fs::write(&path, &updated) {
        return err_response(format!("write: {e}"));
    }

    if let Some(bridge) = w.bridge.get() {
        let bridge_projects: Vec<agentline_bridge::Project> = projects
            .iter()
            .map(|p| agentline_bridge::Project {
                name: p.name.clone(),
                git_url: p.git_url.clone(),
            })
            .collect();
        bridge.update_projects(bridge_projects);
    }

    ok_response("saved")
}

// ─── /api/settings ─────────────────────────────────────────────

#[derive(Serialize)]
struct SettingsOut {
    bridge: BridgeSettingsOut,
    web: WebSettingsOut,
    proxy: ProxySettingsOut,
    log: LogSettingsOut,
}

#[derive(Serialize)]
struct BridgeSettingsOut {
    default_cwd: String,
    session_idle_timeout_secs: u64,
    locale: String,
}

#[derive(Serialize)]
struct WebSettingsOut {
    bind: String,
}

#[derive(Serialize)]
struct ProxySettingsOut {
    http: String,
    https: String,
    no_proxy: String,
}

#[derive(Serialize)]
struct LogSettingsOut {
    level: String,
}

async fn api_settings_get(State(w): State<Web>) -> axum::Json<SettingsOut> {
    let cfg = reload_cfg(&w);
    axum::Json(SettingsOut {
        bridge: BridgeSettingsOut {
            default_cwd: cfg.bridge.default_cwd.clone(),
            session_idle_timeout_secs: cfg.bridge.session_idle_timeout_secs,
            locale: cfg.bridge.locale.clone(),
        },
        web: WebSettingsOut {
            bind: cfg.web.bind.clone(),
        },
        proxy: ProxySettingsOut {
            http: cfg.proxy.http.clone(),
            https: cfg.proxy.https.clone(),
            no_proxy: cfg.proxy.no_proxy.clone(),
        },
        log: LogSettingsOut {
            level: cfg.log.level.clone(),
        },
    })
}

#[derive(Deserialize)]
struct SettingsIn {
    #[serde(default)]
    bridge: Option<BridgeSettingsIn>,
    #[serde(default)]
    web: Option<WebSettingsIn>,
    #[serde(default)]
    proxy: Option<ProxySettingsIn>,
    #[serde(default)]
    log: Option<LogSettingsIn>,
}

#[derive(Deserialize)]
struct BridgeSettingsIn {
    default_cwd: Option<String>,
    session_idle_timeout_secs: Option<u64>,
    locale: Option<String>,
}

#[derive(Deserialize)]
struct WebSettingsIn {
    bind: Option<String>,
}

#[derive(Deserialize)]
struct ProxySettingsIn {
    http: Option<String>,
    https: Option<String>,
    no_proxy: Option<String>,
}

#[derive(Deserialize)]
struct LogSettingsIn {
    level: Option<String>,
}

async fn api_settings_set(
    State(w): State<Web>,
    axum::Json(body): axum::Json<SettingsIn>,
) -> Response {
    let path = config_path(&w);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => return err_response(format!("read config: {e}")),
    };

    let mut updated = text;

    if let Some(ref b) = body.bridge {
        if let Some(ref v) = b.default_cwd {
            updated = set_toml_key(&updated, "bridge", "default_cwd", v);
        }
        if let Some(v) = b.session_idle_timeout_secs {
            updated = set_toml_int(&updated, "bridge", "session_idle_timeout_secs", v);
        }
        if let Some(ref v) = b.locale {
            updated = set_toml_key(&updated, "bridge", "locale", v);
        }
    }
    if let Some(ref w_in) = body.web
        && let Some(ref v) = w_in.bind
    {
        updated = set_toml_key(&updated, "web", "bind", v);
    }
    if let Some(ref p) = body.proxy {
        if let Some(ref v) = p.http {
            updated = set_toml_key(&updated, "proxy", "http", v);
        }
        if let Some(ref v) = p.https {
            updated = set_toml_key(&updated, "proxy", "https", v);
        }
        if let Some(ref v) = p.no_proxy {
            updated = set_toml_key(&updated, "proxy", "no_proxy", v);
        }
    }
    if let Some(ref l) = body.log
        && let Some(ref v) = l.level
    {
        updated = set_toml_key(&updated, "log", "level", v);
    }

    if let Err(e) = std::fs::write(&path, &updated) {
        return err_response(format!("write config: {e}"));
    }
    ok_response("saved")
}

async fn api_restart() -> Response {
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        std::process::exit(0);
    });
    ok_response("restarting")
}

// ─── /api/logs ─────────────────────────────────────────────────

async fn api_logs() -> Response {
    let path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".agentline/agentline.log");
    let text = match std::fs::read_to_string(&path) {
        Ok(s) => {
            let lines: Vec<&str> = s.lines().collect();
            let start = lines.len().saturating_sub(200);
            lines[start..].join("\n")
        }
        Err(e) => format!("(no log yet at {}: {})", path.display(), e),
    };
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response()
}

// ─── TOML helpers ──────────────────────────────────────────────

fn set_toml_backend(text: &str, section: &str, value: &str) -> String {
    let mut result = Vec::new();
    let mut in_section = false;
    let mut done = false;
    let section_header = format!("[{section}]");

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = !done && trimmed == section_header;
        }
        if in_section && !done && trimmed.starts_with("backend") {
            result.push(format!("backend = \"{}\"", value));
            done = true;
            in_section = false;
        } else {
            result.push(line.to_string());
        }
    }

    if !done {
        if !result.iter().any(|l| l.trim() == section_header) {
            result.push(String::new());
            result.push(section_header);
        }
        result.push(format!("backend = \"{}\"", value));
    }

    result.join("\n")
}

fn set_toml_key(text: &str, section: &str, key: &str, value: &str) -> String {
    let section_header = format!("[{section}]");
    let mut result = Vec::new();
    let mut in_section = false;
    let mut done = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = !done && trimmed == section_header;
        }
        if in_section && !done && trimmed.starts_with(key) {
            result.push(format!("{key} = \"{}\"", value));
            done = true;
            in_section = false;
        } else {
            result.push(line.to_string());
        }
    }
    if !done {
        if !result.iter().any(|l| l.trim() == section_header) {
            result.push(String::new());
            result.push(section_header);
        }
        result.push(format!("{key} = \"{}\"", value));
    }
    result.join("\n")
}

fn set_toml_bool(text: &str, section: &str, key: &str, value: bool) -> String {
    let section_header = format!("[{section}]");
    let mut result = Vec::new();
    let mut in_section = false;
    let mut done = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = !done && trimmed == section_header;
        }
        if in_section && !done && trimmed.starts_with(key) {
            result.push(format!("{key} = {}", value));
            done = true;
            in_section = false;
        } else {
            result.push(line.to_string());
        }
    }
    if !done {
        if !result.iter().any(|l| l.trim() == section_header) {
            result.push(String::new());
            result.push(section_header);
        }
        result.push(format!("{key} = {}", value));
    }
    result.join("\n")
}

fn set_toml_int(text: &str, section: &str, key: &str, value: u64) -> String {
    let section_header = format!("[{section}]");
    let mut result = Vec::new();
    let mut in_section = false;
    let mut done = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = !done && trimmed == section_header;
        }
        if in_section && !done && trimmed.starts_with(key) {
            result.push(format!("{key} = {}", value));
            done = true;
            in_section = false;
        } else {
            result.push(line.to_string());
        }
    }
    if !done {
        if !result.iter().any(|l| l.trim() == section_header) {
            result.push(String::new());
            result.push(section_header);
        }
        result.push(format!("{key} = {}", value));
    }
    result.join("\n")
}

fn set_toml_string_array(text: &str, section: &str, key: &str, values: &[String]) -> String {
    let section_header = format!("[{section}]");
    let mut result = Vec::new();
    let mut in_section = false;
    let mut done = false;

    let formatted: Vec<String> = values.iter().map(|v| format!("\"{}\"", v)).collect();
    let array_str = format!("[{}]", formatted.join(", "));

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && !trimmed.starts_with("[[") {
            in_section = !done && trimmed == section_header;
        }
        if in_section && !done && trimmed.starts_with(key) {
            result.push(format!("{key} = {array_str}"));
            done = true;
            in_section = false;
        } else {
            result.push(line.to_string());
        }
    }
    if !done {
        if !result.iter().any(|l| l.trim() == section_header) {
            result.push(String::new());
            result.push(section_header);
        }
        result.push(format!("{key} = {array_str}"));
    }
    result.join("\n")
}

fn set_projects_toml(text: &str, projects: &[ProjectItem]) -> String {
    let mut result: Vec<&str> = Vec::new();
    let mut skip = false;
    for line in text.lines() {
        let t = line.trim();
        if t == "[[projects]]" {
            skip = true;
            continue;
        }
        if skip && (t.starts_with('[') || t.is_empty()) {
            if t.starts_with('[') {
                skip = false;
                result.push(line);
            }
            continue;
        }
        if !skip {
            result.push(line);
        }
    }

    while result.last().is_some_and(|l| l.trim().is_empty()) {
        result.pop();
    }

    let mut out = result.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }

    for p in projects {
        out.push_str(&format!(
            "\n[[projects]]\nname    = \"{}\"\ngit_url = \"{}\"\n",
            p.name.replace('"', "\\\""),
            p.git_url.replace('"', "\\\"")
        ));
    }
    out
}

// ─── response helpers ──────────────────────────────────────────

fn ok_response(msg: &str) -> Response {
    (StatusCode::OK, msg.to_string()).into_response()
}

fn err_response(msg: String) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
}

// ─── login driver ──────────────────────────────────────────────

async fn run_login(
    login: Arc<Mutex<LoginState>>,
    state_path: PathBuf,
    bridge_trigger: Arc<tokio::sync::Notify>,
) {
    let http = match HttpClient::new() {
        Ok(h) => h,
        Err(e) => {
            let mut g = login.lock().await;
            g.state = "failed".into();
            g.message = format!("build http client: {e}");
            return;
        }
    };

    let qr = match request_qr(&http).await {
        Ok(q) => q,
        Err(e) => {
            let mut g = login.lock().await;
            g.state = "failed".into();
            g.message = format!("fetch QR: {e}");
            return;
        }
    };

    {
        let mut g = login.lock().await;
        g.qr_login_url = Some(qr.login_url.clone());
        g.state = "waiting_scan".into();
    }

    let result = match wait_for_scan(&http, &qr).await {
        Ok(r) => r,
        Err(e) => {
            let mut g = login.lock().await;
            g.state = "failed".into();
            g.message = format!("scan: {e}");
            g.qr_login_url = None;
            return;
        }
    };

    let mut app_state = AppState::load_or_default(&state_path).unwrap_or_default();
    app_state.im.wechat.bot_token = Some(result.bot_token);
    app_state.im.wechat.bot_baseurl = result.baseurl;
    app_state.im.wechat.get_updates_buf = String::new();
    if let Err(e) = app_state.save(&state_path) {
        let mut g = login.lock().await;
        g.state = "failed".into();
        g.message = format!("save state: {e}");
        g.qr_login_url = None;
        return;
    }

    {
        let mut g = login.lock().await;
        g.state = "completed".into();
        g.message = "logged in".into();
        g.qr_login_url = None;
    }
    bridge_trigger.notify_one();
}
