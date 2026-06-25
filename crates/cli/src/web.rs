use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Instant;

use agentline_bridge::{
    AgentPluginRegistry, Bridge, CredentialInfo, CredentialUpdate, SessionRegistry,
};
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
    plugins: Arc<AgentPluginRegistry>,
    login: Arc<Mutex<LoginState>>,
    bridge_trigger: Arc<tokio::sync::Notify>,
    registry: Arc<SessionRegistry>,
    bridge: Arc<std::sync::OnceLock<Bridge>>,
    start_time: Instant,
    log_handle: crate::LogReloadHandle,
    update_progress: Arc<AtomicU8>,
    update_status: Arc<AtomicU8>,
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
    plugins: AgentPluginRegistry,
    bind: &str,
    bridge_trigger: Arc<tokio::sync::Notify>,
    registry: Arc<SessionRegistry>,
    log_handle: crate::LogReloadHandle,
) -> (
    tokio::task::JoinHandle<()>,
    Arc<std::sync::OnceLock<Bridge>>,
) {
    let bind = bind.to_string();
    let bridge_holder: Arc<std::sync::OnceLock<Bridge>> = Arc::new(std::sync::OnceLock::new());
    let app_state = Web {
        cfg,
        plugins: Arc::new(plugins),
        login: Arc::new(Mutex::new(LoginState::default())),
        bridge_trigger,
        registry,
        bridge: bridge_holder.clone(),
        start_time: Instant::now(),
        log_handle,
        update_progress: Arc::new(AtomicU8::new(0)),
        update_status: Arc::new(AtomicU8::new(0)),
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
        // Transport
        .route(
            "/api/transport",
            get(api_transport_get).post(api_transport_set),
        )
        // Agents
        .route("/api/agents", get(api_agents))
        .route("/api/agents/config", post(api_agents_config))
        .route("/api/agents/{id}/install", post(api_agents_install))
        .route(
            "/api/agents/{id}/check-update",
            post(api_agents_check_update),
        )
        // MCP (Model Context Protocol)
        .route("/mcp", post(mcp_handler))
        // Projects
        .route("/api/projects", get(api_projects_get).put(api_projects_set))
        // Settings
        .route(
            "/api/settings",
            get(api_settings_get).post(api_settings_set),
        )
        .route("/api/settings/restart", post(api_restart))
        // System update
        .route("/api/system/check-update", get(api_system_check_update))
        .route("/api/system/update", post(api_system_update))
        .route(
            "/api/system/update-progress",
            get(api_system_update_progress),
        )
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

/// Sets HTTP(S)_PROXY/NO_PROXY on this process so the self-update HTTP
/// requests (`reqwest::Client::new()`, which reads them implicitly) go
/// through the same proxy as everything else — falling back to the user's
/// shell profile when `config.toml` leaves `[proxy]` empty.
fn apply_proxy_env(proxy: &crate::config::ProxySection) {
    // Safety: called from request handlers before any concurrent reads of
    // these vars within this process; reqwest only reads them lazily per-request.
    unsafe {
        for (k, v) in crate::run::proxy_env(proxy) {
            std::env::set_var(k, v);
        }
    }
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

// ─── /api/transport ────────────────────────────────────────────

#[derive(Serialize)]
struct TransportOut {
    iroh: IrohTransportOut,
}

#[derive(Serialize)]
struct IrohTransportOut {
    enable: bool,
    token: String,
    relay_url: String,
}

async fn api_transport_get(State(w): State<Web>) -> axum::Json<TransportOut> {
    let cfg = reload_cfg(&w);
    axum::Json(TransportOut {
        iroh: IrohTransportOut {
            enable: cfg.transport.iroh.enable,
            token: cfg.transport.iroh.token.clone(),
            relay_url: cfg.transport.iroh.relay_url.clone(),
        },
    })
}

#[derive(Deserialize, Default)]
struct TransportIn {
    #[serde(default)]
    iroh: Option<IrohTransportIn>,
}

#[derive(Deserialize)]
struct IrohTransportIn {
    enable: Option<bool>,
    token: Option<String>,
    relay_url: Option<String>,
}

async fn api_transport_set(
    State(w): State<Web>,
    axum::Json(body): axum::Json<TransportIn>,
) -> Response {
    let path = config_path(&w);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => return err_response(format!("read config: {e}")),
    };

    let mut updated = text;

    if let Some(ref iroh) = body.iroh {
        if let Some(v) = iroh.enable {
            updated = set_toml_bool(&updated, "transport.iroh", "enable", v);
        }
        if let Some(ref v) = iroh.token {
            updated = set_toml_key(&updated, "transport.iroh", "token", v);
        }
        if let Some(ref v) = iroh.relay_url {
            updated = set_toml_key(&updated, "transport.iroh", "relay_url", v);
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

use std::collections::HashMap;

#[derive(Serialize)]
struct AgentsOut {
    backend: String,
    platform: String,
    list: Vec<AgentItem>,
    credentials: HashMap<String, CredentialInfo>,
    codex_config: CodexCfgOut,
}

#[derive(Serialize)]
struct AgentItem {
    id: String,
    installed: bool,
    version: Option<String>,
    status: String,
}

#[derive(Serialize)]
struct CodexCfgOut {
    model: String,
    sandbox_mode: String,
    approval_mode: String,
}

async fn api_agents(State(w): State<Web>) -> axum::Json<AgentsOut> {
    let cfg = reload_cfg(&w);

    // detect()/auth_status()/read_credential() each shell out (PATH lookup +
    // `--version`/login-state subprocess) — run every plugin's checks on a
    // blocking thread, concurrently, instead of one after another on this task.
    let tasks: Vec<_> = w
        .plugins
        .iter()
        .map(|p| {
            let p = p.clone();
            tokio::task::spawn_blocking(move || {
                let (installed, version) = p.detect();
                let status = if !installed {
                    "not_installed"
                } else if p.auth_status() == agentline_bridge::AuthStatus::Ready {
                    "ready"
                } else {
                    "needs_login"
                };
                (
                    p.id().to_string(),
                    installed,
                    version,
                    status.to_string(),
                    p.read_credential(),
                )
            })
        })
        .collect();

    let mut list = Vec::with_capacity(tasks.len());
    let mut credentials: HashMap<String, CredentialInfo> = HashMap::with_capacity(tasks.len());
    for (id, installed, version, status, credential) in futures::future::join_all(tasks)
        .await
        .into_iter()
        .map(|r| r.expect("agent status check panicked"))
    {
        credentials.insert(id.clone(), credential);
        list.push(AgentItem {
            id,
            installed,
            version,
            status,
        });
    }

    axum::Json(AgentsOut {
        backend: cfg.agent.backend.clone(),
        platform: std::env::consts::OS.to_string(),
        list,
        credentials,
        codex_config: CodexCfgOut {
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
        },
    })
}

#[derive(Deserialize)]
struct AgentConfigIn {
    backend: Option<String>,
    credentials: Option<HashMap<String, CredentialUpdate>>,
    codex: Option<CodexCfgIn>,
}

#[derive(Deserialize)]
struct CodexCfgIn {
    model: Option<String>,
    sandbox_mode: Option<String>,
    approval_mode: Option<String>,
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
    }
    if let Some(ref creds) = body.credentials {
        for (agent_id, update) in creds {
            if let Some(plugin) = w.plugins.iter().find(|p| p.id() == agent_id.as_str()) {
                plugin.sync_credential(update);
            }
        }
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
    let plugin = w.plugins.iter().find(|p| p.id() == id.as_str());
    let args = plugin.and_then(|p| p.install_command());
    let Some(args) = args else {
        return axum::Json(InstallResult {
            success: false,
            error: Some(format!("unknown agent or no installer: {id}")),
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
            if let Some(p) = plugin {
                p.post_install();
            }
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

async fn api_agents_check_update(
    Path(id): Path<String>,
    State(w): State<Web>,
) -> axum::Json<CheckUpdateResult> {
    let plugin = w.plugins.iter().find(|p| p.id() == id.as_str());
    let (installed, current) = plugin.map(|p| p.detect()).unwrap_or((false, None));
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
    /// Detected from the env / login shell (.zshrc, .bashrc, ...) — what
    /// `proxy` above falls back to when a field is left empty. Display only.
    shell_proxy: ProxySettingsOut,
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
        shell_proxy: {
            let shell = agentline_bridge::proxy::detect_shell_proxy();
            ProxySettingsOut {
                http: shell.http.clone(),
                https: shell.https.clone(),
                no_proxy: shell.no_proxy.clone(),
            }
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
        // Apply immediately — no daemon restart needed just to change verbosity.
        w.log_handle.set_level(v);
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

// ─── /api/system/check-update & update ────────────────────────

const GITHUB_RELEASES_API: &str = "https://api.github.com/repos/seven-tt/agentline/releases/latest";

#[derive(Serialize)]
struct SystemUpdateOut {
    has_update: bool,
    current: &'static str,
    latest: String,
}

async fn api_system_check_update(State(w): State<Web>) -> axum::Json<SystemUpdateOut> {
    let current = env!("CARGO_PKG_VERSION");
    apply_proxy_env(&reload_cfg(&w).proxy);
    match fetch_latest_version().await {
        Some(tag) => {
            let has = is_newer_version(&tag, current);
            axum::Json(SystemUpdateOut {
                has_update: has,
                current,
                latest: tag.trim_start_matches('v').to_string(),
            })
        }
        None => axum::Json(SystemUpdateOut {
            has_update: false,
            current,
            latest: current.to_string(),
        }),
    }
}

async fn api_system_update(State(w): State<Web>) -> Response {
    let current = env!("CARGO_PKG_VERSION");
    apply_proxy_env(&reload_cfg(&w).proxy);

    let info = match fetch_release_info().await {
        Some(info) if is_newer_version(&info.tag, current) => info,
        _ => {
            return (StatusCode::OK, "already up to date").into_response();
        }
    };

    w.update_progress.store(0, Ordering::Relaxed);
    w.update_status.store(1, Ordering::Relaxed); // downloading

    let progress = w.update_progress.clone();
    let status = w.update_status.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = do_system_update(&info, &progress, &status) {
            tracing::error!(error=%e, "system update failed");
            status.store(4, Ordering::Relaxed); // error
        }
    });

    (StatusCode::ACCEPTED, "updating").into_response()
}

async fn api_system_update_progress(State(w): State<Web>) -> axum::Json<serde_json::Value> {
    let status_val = w.update_status.load(Ordering::Relaxed);
    let percent = w.update_progress.load(Ordering::Relaxed);
    let status_str = match status_val {
        1 => "downloading",
        2 => "installing",
        3 => "done",
        4 => "error",
        _ => "idle",
    };
    axum::Json(serde_json::json!({
        "status": status_str,
        "percent": percent,
    }))
}

struct ReleaseDownloadInfo {
    tag: String,
    asset_url: String,
}

async fn fetch_latest_version() -> Option<String> {
    let client = reqwest::Client::new();
    let resp = client
        .get(GITHUB_RELEASES_API)
        .header("User-Agent", "agentline")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .ok()?;
    let json: serde_json::Value = resp.json().await.ok()?;
    json.get("tag_name")?.as_str().map(|s| s.to_string())
}

async fn fetch_release_info() -> Option<ReleaseDownloadInfo> {
    let client = reqwest::Client::new();
    let resp = client
        .get(GITHUB_RELEASES_API)
        .header("User-Agent", "agentline")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .ok()?;
    let json: serde_json::Value = resp.json().await.ok()?;
    let tag = json.get("tag_name")?.as_str()?.to_string();

    let (target, ext) = if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            ("mac-arm64", ".dmg")
        } else {
            ("mac-x64", ".dmg")
        }
    } else if cfg!(target_os = "linux") {
        if cfg!(target_arch = "aarch64") {
            ("linux-arm64", ".deb")
        } else {
            ("linux-x64", ".deb")
        }
    } else {
        ("win-x64", "-setup.exe")
    };

    let assets = json.get("assets")?.as_array()?;
    let asset_url = assets.iter().find_map(|a| {
        let name = a.get("name")?.as_str()?;
        if name.contains(target) && name.ends_with(ext) {
            a.get("browser_download_url")?
                .as_str()
                .map(|s| s.to_string())
        } else {
            None
        }
    })?;

    Some(ReleaseDownloadInfo { tag, asset_url })
}

fn is_newer_version(remote_tag: &str, local: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.trim_start_matches('v')
            .split('.')
            .filter_map(|s| s.parse().ok())
            .collect()
    };
    let r = parse(remote_tag);
    let l = parse(local);
    for i in 0..3 {
        let rv = r.get(i).copied().unwrap_or(0);
        let lv = l.get(i).copied().unwrap_or(0);
        if rv > lv {
            return true;
        }
        if rv < lv {
            return false;
        }
    }
    false
}

fn do_system_update(
    info: &ReleaseDownloadInfo,
    progress: &Arc<AtomicU8>,
    status: &Arc<AtomicU8>,
) -> anyhow::Result<()> {
    use std::process::Command;

    let tmp_dmg = std::path::Path::new("/tmp/agentline-update.dmg");
    let tmp_dir = std::path::Path::new("/tmp/agentline-update");
    let tmp_mount = std::path::Path::new("/tmp/agentline-mount");

    let _ = std::fs::remove_file(tmp_dmg);
    let _ = std::fs::remove_dir_all(tmp_dir);
    let _ = Command::new("hdiutil")
        .args(["detach", "/tmp/agentline-mount", "-quiet", "-force"])
        .status();

    tracing::info!(tag=%info.tag, "downloading update");
    status.store(1, Ordering::Relaxed);

    let rt = tokio::runtime::Handle::try_current().ok().and_then(|h| {
        tokio::task::block_in_place(|| {
            h.block_on(async {
                let client = reqwest::Client::new();
                client
                    .get(&info.asset_url)
                    .header("User-Agent", "agentline")
                    .send()
                    .await
                    .ok()
            })
        })
    });

    let resp = rt.ok_or_else(|| anyhow::anyhow!("download request failed"))?;
    let content_len = resp.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;
    let mut last_pct: u8 = 0;

    let mut file = std::fs::File::create(tmp_dmg)?;
    let mut resp = resp;
    loop {
        let chunk = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(resp.chunk())
        })?;
        match chunk {
            Some(bytes) => {
                std::io::Write::write_all(&mut file, &bytes)?;
                downloaded += bytes.len() as u64;
                if let Some(pct_val) = (downloaded * 100).checked_div(content_len) {
                    let pct = pct_val.min(100) as u8;
                    if pct != last_pct {
                        last_pct = pct;
                        progress.store(pct, Ordering::Relaxed);
                    }
                }
            }
            None => break,
        }
    }
    drop(file);

    tracing::info!("installing update");
    status.store(2, Ordering::Relaxed);

    // Mount DMG
    let cmd_status = Command::new("hdiutil")
        .args([
            "attach",
            tmp_dmg.to_str().unwrap(),
            "-nobrowse",
            "-quiet",
            "-mountpoint",
            tmp_mount.to_str().unwrap(),
        ])
        .status()?;
    if !cmd_status.success() {
        anyhow::bail!("hdiutil attach failed");
    }

    // Copy .app from mounted DMG
    std::fs::create_dir_all(tmp_dir)?;
    let cp_status = Command::new("cp")
        .args([
            "-R",
            tmp_mount
                .join("AgentlineTray.app")
                .to_str()
                .unwrap_or_default(),
            tmp_dir.to_str().unwrap(),
        ])
        .status()?;

    let _ = Command::new("hdiutil")
        .args(["detach", tmp_mount.to_str().unwrap(), "-quiet"])
        .status();

    if !cp_status.success() {
        anyhow::bail!("cp from DMG failed");
    }

    let extracted_app = tmp_dir.join("AgentlineTray.app");
    if !extracted_app.exists() {
        anyhow::bail!("AgentlineTray.app not found in DMG");
    }

    let current_exe = std::env::current_exe()?;
    let current_app = current_exe
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .ok_or_else(|| anyhow::anyhow!("cannot determine .app path"))?;

    let app_parent = current_app
        .parent()
        .ok_or_else(|| anyhow::anyhow!("no parent of .app"))?;
    let app_name = current_app
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("no .app filename"))?
        .to_owned();

    let backup = app_parent.join("AgentlineTray.app.bak");
    let _ = std::fs::remove_dir_all(&backup);
    std::fs::rename(current_app, &backup)?;
    if let Err(e) = std::fs::rename(&extracted_app, app_parent.join(&app_name)) {
        let _ = std::fs::rename(&backup, current_app);
        return Err(e.into());
    }
    let _ = std::fs::remove_dir_all(&backup);
    let _ = std::fs::remove_file(tmp_dmg);
    let _ = std::fs::remove_dir_all(tmp_dir);

    status.store(3, Ordering::Relaxed);
    tracing::info!("update installed, restarting");
    let new_exe = app_parent
        .join(&app_name)
        .join("Contents/MacOS/agentline-tray");
    let _ = Command::new(new_exe).spawn();
    std::process::exit(0);
}

// ---------------------------------------------------------------------------
// MCP (Model Context Protocol) endpoint
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct McpRequest {
    jsonrpc: String,
    id: Option<serde_json::Value>,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

async fn mcp_handler(
    State(w): State<Web>,
    axum::Json(req): axum::Json<McpRequest>,
) -> axum::Json<serde_json::Value> {
    let id = req.id.clone().unwrap_or(serde_json::Value::Null);

    if req.jsonrpc != "2.0" {
        return axum::Json(mcp_error(id, -32600, "invalid jsonrpc version"));
    }

    match req.method.as_str() {
        "initialize" => axum::Json(mcp_result(
            id,
            serde_json::json!({
                "protocolVersion": "2025-03-26",
                "serverInfo": {
                    "name": "agentline",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": {
                    "tools": {}
                }
            }),
        )),

        "notifications/initialized" => axum::Json(mcp_result(id, serde_json::json!({}))),

        "tools/list" => axum::Json(mcp_result(
            id,
            serde_json::json!({
                "tools": [{
                    "name": "list_projects",
                    "description": "List all configured projects with their name and git URL",
                    "inputSchema": {
                        "type": "object",
                        "properties": {},
                        "required": []
                    }
                }]
            }),
        )),

        "tools/call" => {
            let tool_name = req
                .params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            match tool_name {
                "list_projects" => {
                    let cfg = reload_cfg(&w);
                    let projects: Vec<serde_json::Value> = cfg
                        .projects
                        .iter()
                        .map(|p| {
                            serde_json::json!({
                                "name": p.name,
                                "git_url": p.git_url,
                            })
                        })
                        .collect();

                    axum::Json(mcp_result(
                        id,
                        serde_json::json!({
                            "content": [{
                                "type": "text",
                                "text": serde_json::to_string(&projects).unwrap_or_default()
                            }]
                        }),
                    ))
                }
                _ => axum::Json(mcp_error(id, -32602, &format!("unknown tool: {tool_name}"))),
            }
        }

        _ => axum::Json(mcp_error(
            id,
            -32601,
            &format!("method not found: {}", req.method),
        )),
    }
}

fn mcp_result(id: serde_json::Value, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

fn mcp_error(id: serde_json::Value, code: i32, message: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        },
    })
}
