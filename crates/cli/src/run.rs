use crate::config::{AppConfig, ProxySection};
use crate::state::{AppState, FileCursorPersist};
use agentline_bridge::{
    AgentBackend, AgentFactory, Bridge, BridgeConfig, PluginAgentFactory, SessionRegistry,
    SourceRouter,
};
use agentline_im_dingtalk::{DingtalkChannel, OpenParams, StreamConfig};
use agentline_im_feishu::{FeishuChannel, FeishuConfig};
use agentline_im_telegram::{TelegramChannel, TelegramConfig};
use agentline_im_wechat::{HttpClient, WechatChannel};
use anyhow::{Context, Result, anyhow, bail};
use std::sync::Arc;
use std::time::Duration;

pub async fn run(cfg: AppConfig, acp: bool, log_handle: crate::LogReloadHandle) -> Result<()> {
    if acp {
        return run_acp(cfg).await;
    }

    // Single-instance guard: refuse to start if another daemon already holds
    // the lock. Without this, a tray respawn bug (or a stray manual launch)
    // can leave two daemons both polling the same IM token → every message is
    // answered twice. The lock is released automatically when the process exits.
    acquire_single_instance_lock(&cfg)?;

    let bridge_trigger = std::sync::Arc::new(tokio::sync::Notify::new());
    let registry = Arc::new(SessionRegistry::new());

    let web_bridge_holder = if cfg.web.enable {
        let cfg_for_web = std::sync::Arc::new(cfg.clone());
        let (_handle, holder) = crate::web::start(
            cfg_for_web,
            build_plugin_registry(),
            &cfg.web.bind,
            std::sync::Arc::clone(&bridge_trigger),
            Arc::clone(&registry),
            log_handle,
        );
        Some(holder)
    } else {
        None
    };

    loop {
        match try_run_bridge(&cfg, Arc::clone(&registry), web_bridge_holder.clone()).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                tracing::error!(error=%e, "bridge unavailable; waiting for login trigger");
                if !cfg.web.enable {
                    return Err(e);
                }
            }
        }
        // Wait until web layer signals (e.g. login completed).
        bridge_trigger.notified().await;
        tracing::info!("bridge trigger received, retrying bridge startup");
    }
}

/// Acquire an exclusive, advisory lock on `<state_dir>/agentline.lock`. If
/// another `agentline run` already holds it, bail out so we never run two
/// daemons against the same IM account. The lock fd is intentionally leaked so
/// the OS holds it until this process exits.
fn acquire_single_instance_lock(cfg: &AppConfig) -> Result<()> {
    use std::io::Write;
    let path = crate::config::expand_tilde(&cfg.bridge.state_dir).join("agentline.lock");
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).ok();
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("open lock file {}", path.display()))?;
    if file.try_lock().is_err() {
        bail!(
            "another agentline instance is already running (lock held: {}). \
             Refusing to start a second daemon.",
            path.display()
        );
    }
    // Write our PID so the tray can kill us if we become stale.
    let _ = file.set_len(0);
    let mut f = &file;
    let _ = f.write_all(format!("{}", std::process::id()).as_bytes());
    let _ = f.flush();
    std::mem::forget(file); // hold the lock for the process lifetime
    Ok(())
}

async fn try_run_bridge(
    cfg: &AppConfig,
    registry: Arc<SessionRegistry>,
    web_bridge: Option<Arc<std::sync::OnceLock<Bridge>>>,
) -> Result<()> {
    rust_i18n::set_locale(&cfg.bridge.locale);

    agentline_agent_acp::cleanup_orphaned_agent(&agent_pid_path(cfg));

    let enabled = cfg.im.enabled_backends();
    if enabled.is_empty() {
        bail!("no IM backend enabled — set enable = true in at least one [im.*] section");
    }

    let plugins = build_plugin_registry();
    let agent_section: toml::Value = toml::Value::try_from(&cfg.agent)
        .unwrap_or_else(|_| toml::Value::Table(Default::default()));
    let factory = Arc::new(PluginAgentFactory::new(
        plugins,
        proxy_env(&cfg.proxy),
        Some(agent_pid_path(cfg)),
        agent_section,
    ));
    let agent: Arc<dyn AgentBackend> = factory
        .build(&cfg.agent.backend)
        .await
        .map_err(|e| anyhow!("{e}"))?;

    let (default_cwd, session_base_dir) = if cfg.bridge.default_cwd.trim().is_empty() {
        let state_dir = crate::config::expand_tilde(&cfg.bridge.state_dir);
        let base = state_dir.join("agents").join(&cfg.agent.backend);
        (base.clone(), Some(base))
    } else {
        (cfg.resolved_cwd(), None)
    };
    if let Err(e) = std::fs::create_dir_all(&default_cwd) {
        tracing::error!(error=%e, cwd=%default_cwd.display(), "could not create default working dir");
    }

    let projects: Vec<agentline_bridge::Project> = cfg
        .projects
        .iter()
        .map(|p| agentline_bridge::Project {
            name: p.name.clone(),
            git_url: p.git_url.clone(),
        })
        .collect();

    // Build all IM adapters and register them with a single SourceRouter.
    let mut router = SourceRouter::new();
    let mut registered = 0usize;

    for im_name in &enabled {
        match build_im_source(cfg, im_name).await {
            Ok(adapter) => {
                router.register_im(adapter);
                registry.update(
                    im_name,
                    agentline_bridge::ImSnapshot {
                        healthy: true,
                        sessions: vec![],
                    },
                );
                registered += 1;
            }
            Err(e) => {
                tracing::error!(im = %im_name, error = %e, "failed to build IM adapter; skipping");
                registry.update(
                    im_name,
                    agentline_bridge::ImSnapshot {
                        healthy: false,
                        sessions: vec![],
                    },
                );
            }
        }
    }

    if registered == 0 {
        bail!("all enabled IM backends failed to start");
    }

    let bridge_cfg = BridgeConfig {
        default_cwd: default_cwd.clone(),
        typing_interval: Duration::from_secs(30),
        session_idle_timeout: Duration::from_secs(cfg.bridge.session_idle_timeout_secs),
        agent_name: cfg.agent.backend.clone(),
        projects: projects.clone(),
        session_base_dir: session_base_dir.clone(),
        registry: Some(registry.clone()),
        config_path: cfg.config_path.clone(),
    };

    let (bridge, actor_handle) = Bridge::from_router(
        router,
        agent.clone(),
        bridge_cfg,
        std::sync::Arc::new(agentline_im_core::ImInboundHandler::new()),
    );
    if let Some(ref holder) = web_bridge {
        let _ = holder.set(bridge.clone());
    }
    // Start transport listeners (unix socket, iroh).
    let _transport_handles = start_transports(cfg, &bridge)?;

    let _bridge = bridge.with_agent_factory(factory.clone());

    tracing::info!(
        cwd = %default_cwd.display(),
        ims = ?enabled,
        agent = %cfg.agent.backend,
        "agentline ready; {registered} adapter(s) registered via SourceRouter",
    );

    let agent_for_shutdown = agent;

    tokio::select! {
        result = actor_handle => {
            if let Err(e) = result {
                tracing::error!(error = %e, "bridge actor task panicked");
            }
            agent_for_shutdown.shutdown().await;
        }
        sig = shutdown_signal() => {
            tracing::info!(signal = sig, "shutdown signal received; terminating agent");
            agent_for_shutdown.shutdown().await;
        }
    }
    Ok(())
}

async fn run_acp(cfg: AppConfig) -> Result<()> {
    use agentline_bridge::acp_server::{AcpSource, serve_acp};

    rust_i18n::set_locale(&cfg.bridge.locale);

    agentline_agent_acp::cleanup_orphaned_agent(&agent_pid_path(&cfg));

    let plugins = build_plugin_registry();
    let agent_section: toml::Value = toml::Value::try_from(&cfg.agent)
        .unwrap_or_else(|_| toml::Value::Table(Default::default()));
    let factory = Arc::new(PluginAgentFactory::new(
        plugins,
        proxy_env(&cfg.proxy),
        Some(agent_pid_path(&cfg)),
        agent_section,
    ));
    let agent: Arc<dyn AgentBackend> = factory
        .build(&cfg.agent.backend)
        .await
        .map_err(|e| anyhow!("{e}"))?;

    let default_cwd = if cfg.bridge.default_cwd.trim().is_empty() {
        let state_dir = crate::config::expand_tilde(&cfg.bridge.state_dir);
        state_dir.join("agents").join(&cfg.agent.backend)
    } else {
        cfg.resolved_cwd()
    };
    if let Err(e) = std::fs::create_dir_all(&default_cwd) {
        tracing::error!(error=%e, cwd=%default_cwd.display(), "could not create default working dir");
    }

    let router = SourceRouter::new();
    let (source, out_rx) = AcpSource::new();
    router.register_source(source.clone());

    let bridge_cfg = BridgeConfig {
        default_cwd,
        typing_interval: Duration::from_secs(30),
        session_idle_timeout: Duration::from_secs(cfg.bridge.session_idle_timeout_secs),
        agent_name: cfg.agent.backend.clone(),
        projects: vec![],
        session_base_dir: None,
        registry: None,
        config_path: cfg.config_path.clone(),
    };

    let (bridge, actor_handle) = Bridge::from_router(
        router,
        agent.clone(),
        bridge_cfg,
        std::sync::Arc::new(agentline_im_core::ImInboundHandler::new()),
    );
    let _bridge = bridge.clone().with_agent_factory(factory);

    tracing::info!(agent = %cfg.agent.backend, "agentline ACP server starting on stdio");

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            tokio::select! {
                result = serve_acp(
                    bridge,
                    source,
                    out_rx,
                    tokio::io::stdin(),
                    tokio::io::stdout(),
                ) => {
                    if let Err(e) = result {
                        tracing::error!(error=%e, "ACP server error");
                    }
                }
                _ = actor_handle => {
                    tracing::info!("bridge actor exited");
                }
            }
            agent.shutdown().await;
        })
        .await;
    Ok(())
}

fn build_plugin_registry() -> agentline_bridge::AgentPluginRegistry {
    vec![
        agentline_agent_claude_code::plugin(),
        agentline_agent_kimi::plugin(),
        agentline_agent_qoder::plugin(),
        agentline_agent_opencode::plugin(),
        agentline_agent_kiro::plugin(),
        agentline_agent_gemini::plugin(),
        agentline_agent_hermes::plugin(),
        agentline_agent_codex::plugin(),
        agentline_agent_acp::plugin(),
    ]
}

/// Build an IM adapter implementing InputSource + ImAdapter for the new
/// SourceRouter architecture. The adapter is created in deferred-start mode
/// — `InputSource::start()` is called later by the SourceRouter.
async fn build_im_source(
    cfg: &AppConfig,
    name: &str,
) -> Result<Arc<dyn agentline_bridge::ImAdapter>> {
    match name {
        "wechat" => build_wechat_source(cfg).await,
        "dingtalk" => build_dingtalk_source(cfg).await,
        "feishu" => build_feishu_source(cfg).await,
        "telegram" => build_telegram_source(cfg),
        other => bail!(
            "im backend {:?}: unsupported (try `wechat`, `dingtalk`, `feishu`, or `telegram`)",
            other
        ),
    }
}

async fn build_dingtalk_source(cfg: &AppConfig) -> Result<Arc<dyn agentline_bridge::ImAdapter>> {
    if cfg.im.dingtalk.client_id.is_empty() || cfg.im.dingtalk.client_secret.is_empty() {
        bail!(
            "dingtalk requires `im.dingtalk.client_id` and `im.dingtalk.client_secret` in config"
        );
    }
    let stream_cfg = StreamConfig {
        open: OpenParams {
            client_id: cfg.im.dingtalk.client_id.clone(),
            client_secret: cfg.im.dingtalk.client_secret.clone(),
            user_agent: format!("agentline/{}", env!("CARGO_PKG_VERSION")),
        },
        allowed_users: cfg.im.dingtalk.allowed_users.clone(),
        buffer: 32,
        card_template_id: cfg.im.dingtalk.card_template_id.clone(),
    };
    let channel = DingtalkChannel::new(stream_cfg)
        .await
        .map_err(|e| anyhow!("dingtalk: {e}"))?;
    Ok(Arc::new(channel))
}

fn build_telegram_source(cfg: &AppConfig) -> Result<Arc<dyn agentline_bridge::ImAdapter>> {
    if cfg.im.telegram.bot_token.is_empty() {
        bail!("telegram requires `im.telegram.bot_token` in config");
    }
    let tg_cfg = TelegramConfig {
        bot_token: cfg.im.telegram.bot_token.clone(),
        allowed_users: cfg.im.telegram.allowed_users.clone(),
        api_base: cfg.im.telegram.api_base.clone(),
        proxy: resolve_https_proxy(&cfg.proxy),
    };
    let channel = TelegramChannel::new(tg_cfg).map_err(|e| anyhow!("telegram: {e}"))?;
    Ok(Arc::new(channel))
}

async fn build_feishu_source(cfg: &AppConfig) -> Result<Arc<dyn agentline_bridge::ImAdapter>> {
    if cfg.im.feishu.app_id.is_empty() || cfg.im.feishu.app_secret.is_empty() {
        bail!("feishu requires `im.feishu.app_id` and `im.feishu.app_secret` in config");
    }
    let feishu_cfg = FeishuConfig {
        app_id: cfg.im.feishu.app_id.clone(),
        app_secret: cfg.im.feishu.app_secret.clone(),
        allowed_users: cfg.im.feishu.allowed_users.clone(),
    };
    let channel = FeishuChannel::new(feishu_cfg)
        .await
        .map_err(|e| anyhow!("feishu: {e}"))?;
    Ok(Arc::new(channel))
}

async fn build_wechat_source(cfg: &AppConfig) -> Result<Arc<dyn agentline_bridge::ImAdapter>> {
    let state_path = cfg.state_path()?;
    let state = AppState::load_or_default(&state_path).context("load state")?;
    let token = state
        .im
        .wechat
        .bot_token
        .clone()
        .ok_or_else(|| anyhow!("no bot_token in state — run `agentline login` first"))?;
    let cursor = state.im.wechat.get_updates_buf.clone();

    let http = HttpClient::new().context("build http client")?;
    http.set_token(token).await;
    if let Some(b) = state.im.wechat.bot_baseurl.clone() {
        http.set_base_url(b).await;
    }
    let persist = FileCursorPersist::new(state_path.clone());
    let channel = WechatChannel::new(
        http,
        cursor,
        persist,
        cfg.im.wechat.allowed_users.clone(),
        state.im.wechat.context_tokens.clone(),
    );
    Ok(Arc::new(channel))
}

/// Resolve when the process is asked to terminate (Ctrl-C / SIGTERM).
async fn shutdown_signal() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => tokio::select! {
                _ = tokio::signal::ctrl_c() => "SIGINT",
                _ = term.recv() => "SIGTERM",
            },
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                "SIGINT"
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        "ctrl_c"
    }
}

fn agent_pid_path(cfg: &AppConfig) -> std::path::PathBuf {
    crate::config::expand_tilde(&cfg.bridge.state_dir).join("agent.pid")
}

fn start_transports(cfg: &AppConfig, bridge: &Bridge) -> Result<Vec<std::thread::JoinHandle<()>>> {
    let mut handles = Vec::new();
    let token = if cfg.transport.token.is_empty() {
        None
    } else {
        Some(cfg.transport.token.clone())
    };
    let cwd = bridge.config().default_cwd.clone();

    #[cfg(unix)]
    if !cfg.transport.unix_socket.is_empty() {
        let path = crate::config::expand_tilde(&cfg.transport.unix_socket);
        let listener = agentline_transport::UnixSocketListener::bind(&path)
            .map_err(|e| anyhow!("unix transport: {e}"))?;
        handles.push(agentline_transport::spawn_transport(
            bridge.clone(),
            std::sync::Arc::new(listener),
            token.clone(),
            cwd.clone(),
        ));
    }

    #[cfg(feature = "iroh")]
    if cfg.transport.iroh.enable {
        let bridge = bridge.clone();
        let key_path = crate::config::expand_tilde(&cfg.bridge.state_dir).join("iroh.key");
        let rt = tokio::runtime::Handle::current();
        let listener = rt
            .block_on(agentline_transport_iroh::IrohListener::new(
                &cfg.transport.iroh.secret_key,
                &key_path,
                &cfg.transport.iroh.relay_url,
            ))
            .map_err(|e| anyhow!("iroh transport: {e}"))?;
        handles.push(agentline_transport::spawn_transport(
            bridge,
            std::sync::Arc::new(listener),
            token.clone(),
            cwd.clone(),
        ));
    }

    Ok(handles)
}

fn resolve_https_proxy(proxy: &ProxySection) -> String {
    let shell = agentline_bridge::proxy::detect_shell_proxy();
    if !proxy.https.is_empty() {
        proxy.https.clone()
    } else if !proxy.http.is_empty() {
        proxy.http.clone()
    } else if !shell.https.is_empty() {
        shell.https.clone()
    } else {
        shell.http.clone()
    }
}

fn proxy_env(proxy: &ProxySection) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let shell = agentline_bridge::proxy::detect_shell_proxy();

    // Empty config field = fall back to whatever the user already has set in
    // their shell (env or .zshrc/.bashrc via detect_shell_proxy), so agentline
    // matches their ambient proxy setup instead of silently going direct.
    let http_val = if !proxy.http.is_empty() {
        proxy.http.clone()
    } else {
        shell.http.clone()
    };
    if !http_val.is_empty() {
        pairs.push(("HTTP_PROXY".into(), http_val.clone()));
        pairs.push(("http_proxy".into(), http_val));
    }

    let https_val = if !proxy.https.is_empty() {
        proxy.https.clone()
    } else if !proxy.http.is_empty() {
        proxy.http.clone()
    } else if !shell.https.is_empty() {
        shell.https.clone()
    } else {
        shell.http.clone()
    };
    if !https_val.is_empty() {
        pairs.push(("HTTPS_PROXY".into(), https_val.clone()));
        pairs.push(("https_proxy".into(), https_val));
    }

    let no_proxy = agentline_bridge::proxy::build_no_proxy_with(&proxy.no_proxy);
    pairs.push(("NO_PROXY".into(), no_proxy.clone()));
    pairs.push(("no_proxy".into(), no_proxy));

    pairs
}
