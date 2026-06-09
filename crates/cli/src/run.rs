use crate::config::{AppConfig, ProxySection};
use crate::state::{AppState, FileCursorPersist};
use agentline_agent_acp::{AcpBackend, AcpBackendConfig};
use agentline_agent_claude_code::{ClaudeCodeConfig, spawn as spawn_claude_code};
use agentline_agent_codex::{ApprovalMode, CodexBackend, CodexConfig, SandboxMode};
use agentline_agent_gemini::{GeminiConfig, spawn as spawn_gemini};
use agentline_agent_hermes::{HermesConfig, spawn as spawn_hermes};
use agentline_agent_kimi::{KimiConfig, spawn as spawn_kimi};
use agentline_agent_kiro::{KiroConfig, spawn as spawn_kiro};
use agentline_agent_opencode::{OpencodeConfig, spawn as spawn_opencode};
use agentline_agent_qoder::{QoderConfig, spawn as spawn_qoder};
use agentline_bridge::{AgentBackend, Bridge, BridgeConfig, ImChannel, SessionRegistry};
use agentline_im_dingtalk::{DingtalkChannel, OpenParams, StreamConfig};
use agentline_im_feishu::{FeishuChannel, FeishuConfig};
use agentline_im_telegram::{TelegramChannel, TelegramConfig};
use agentline_im_wechat::{HttpClient, WechatChannel};
use anyhow::{Context, Result, anyhow, bail};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

pub async fn run(cfg: AppConfig) -> Result<()> {
    // Single-instance guard: refuse to start if another daemon already holds
    // the lock. Without this, a tray respawn bug (or a stray manual launch)
    // can leave two daemons both polling the same IM token → every message is
    // answered twice. The lock is released automatically when the process exits.
    acquire_single_instance_lock(&cfg)?;

    let bridge_trigger = std::sync::Arc::new(tokio::sync::Notify::new());
    let registry = Arc::new(SessionRegistry::new());

    if cfg.web.enable {
        let cfg_for_web = std::sync::Arc::new(cfg.clone());
        crate::web::start(
            cfg_for_web,
            &cfg.web.bind,
            std::sync::Arc::clone(&bridge_trigger),
            Arc::clone(&registry),
        );
    }

    loop {
        match try_run_bridge(&cfg, Arc::clone(&registry)).await {
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

async fn try_run_bridge(cfg: &AppConfig, registry: Arc<SessionRegistry>) -> Result<()> {
    rust_i18n::set_locale(&cfg.bridge.locale);

    agentline_agent_acp::cleanup_orphaned_agent(&agent_pid_path(cfg));

    let enabled = cfg.im.enabled_backends();
    if enabled.is_empty() {
        bail!("no IM backend enabled — set enable = true in at least one [im.*] section");
    }

    let agent: Arc<dyn AgentBackend> = build_agent(cfg).await?;

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

    // Spawn a Bridge per enabled IM — each runs in its own tokio task,
    // all sharing the same agent backend.
    let mut abort_handles: Vec<tokio::task::AbortHandle> = Vec::new();
    let mut join_handles: Vec<tokio::task::JoinHandle<Result<()>>> = Vec::new();

    for im_name in &enabled {
        match build_im(cfg, im_name).await {
            Ok((im, inbound_rx, typing_ms)) => {
                registry.update(
                    im_name,
                    agentline_bridge::ImSnapshot {
                        healthy: true,
                        sessions: vec![],
                    },
                );

                let bridge_cfg = BridgeConfig {
                    default_cwd: default_cwd.clone(),
                    typing_interval: Duration::from_millis(typing_ms),
                    session_idle_timeout: Duration::from_secs(cfg.bridge.session_idle_timeout_secs),
                    agent_name: cfg.agent.backend.clone(),
                    projects: projects.clone(),
                    session_base_dir: session_base_dir.clone(),
                    im_id: im_name.to_string(),
                    registry: Some(registry.clone()),
                };

                let bridge = Bridge::new(im, agent.clone(), bridge_cfg);
                let name = im_name.to_string();
                let handle = tokio::spawn(async move {
                    tracing::info!(im = %name, "bridge loop started");
                    bridge
                        .run(inbound_rx)
                        .await
                        .map_err(|e| anyhow!("bridge[{name}]: {e}"))
                });
                abort_handles.push(handle.abort_handle());
                join_handles.push(handle);
            }
            Err(e) => {
                tracing::error!(im = %im_name, error = %e, "failed to build IM channel; skipping");
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

    if join_handles.is_empty() {
        bail!("all enabled IM backends failed to start");
    }

    tracing::info!(
        cwd = %default_cwd.display(),
        ims = ?enabled,
        agent = %cfg.agent.backend,
        "agentline ready; {} bridge(s) running",
        join_handles.len()
    );

    let agent_for_shutdown = agent;
    let join_future = futures::future::join_all(join_handles);

    tokio::select! {
        results = join_future => {
            for r in results {
                match r {
                    Ok(Err(e)) => tracing::error!(error = %e, "bridge exited with error"),
                    Err(e) => tracing::error!(error = %e, "bridge task panicked"),
                    Ok(Ok(())) => {}
                }
            }
            agent_for_shutdown.shutdown().await;
        }
        sig = shutdown_signal() => {
            tracing::info!(signal = sig, "shutdown signal received; terminating agent");
            for h in &abort_handles {
                h.abort();
            }
            agent_for_shutdown.shutdown().await;
        }
    }
    Ok(())
}

async fn build_agent(cfg: &AppConfig) -> Result<Arc<dyn AgentBackend>> {
    match cfg.agent.backend.as_str() {
        "claude-code" => Ok(Arc::new(build_claude_code(cfg).await?)),
        "kimi" => Ok(Arc::new(build_kimi(cfg).await?)),
        "qoder" => Ok(Arc::new(build_qoder(cfg).await?)),
        "opencode" => Ok(Arc::new(build_opencode(cfg).await?)),
        "kiro" => Ok(Arc::new(build_kiro(cfg).await?)),
        "gemini" => Ok(Arc::new(build_gemini(cfg).await?)),
        "hermes" => Ok(Arc::new(build_hermes(cfg).await?)),
        "codex" => Ok(Arc::new(build_codex(cfg).await?)),
        "acp" => Ok(Arc::new(build_generic_acp(cfg).await?)),
        other => bail!(
            "agent.backend = {:?}: unsupported (try `claude-code`, `kimi`, `qoder`, `opencode`, `kiro`, `gemini`, `hermes`, `codex`, or `acp`)",
            other
        ),
    }
}

async fn build_im(
    cfg: &AppConfig,
    name: &str,
) -> Result<(
    Arc<dyn ImChannel>,
    mpsc::Receiver<agentline_bridge::types::InboundMessage>,
    u64,
)> {
    match name {
        "wechat" => build_wechat(cfg).await,
        "dingtalk" => build_dingtalk(cfg).await,
        "feishu" => build_feishu(cfg).await,
        "telegram" => build_telegram(cfg).await,
        other => bail!(
            "im backend {:?}: unsupported (try `wechat`, `dingtalk`, `feishu`, or `telegram`)",
            other
        ),
    }
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

async fn build_wechat(
    cfg: &AppConfig,
) -> Result<(
    Arc<dyn ImChannel>,
    mpsc::Receiver<agentline_bridge::types::InboundMessage>,
    u64,
)> {
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
    let (channel, inbound_rx, _poll_handle, _cursor_cell) = WechatChannel::start(
        http,
        cursor,
        persist,
        cfg.im.wechat.allowed_users.clone(),
        state.im.wechat.context_tokens.clone(),
    );
    Ok((
        Arc::new(channel),
        inbound_rx,
        cfg.im.wechat.typing_interval_ms,
    ))
}

async fn build_dingtalk(
    cfg: &AppConfig,
) -> Result<(
    Arc<dyn ImChannel>,
    mpsc::Receiver<agentline_bridge::types::InboundMessage>,
    u64,
)> {
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
    };
    let (channel, inbound_rx, _handle) =
        DingtalkChannel::start(stream_cfg).map_err(|e| anyhow!("dingtalk: {e}"))?;
    Ok((Arc::new(channel), inbound_rx, 60_000))
}

async fn build_feishu(
    cfg: &AppConfig,
) -> Result<(
    Arc<dyn ImChannel>,
    mpsc::Receiver<agentline_bridge::types::InboundMessage>,
    u64,
)> {
    if cfg.im.feishu.app_id.is_empty() || cfg.im.feishu.app_secret.is_empty() {
        bail!("feishu requires `im.feishu.app_id` and `im.feishu.app_secret` in config");
    }
    let feishu_cfg = FeishuConfig {
        app_id: cfg.im.feishu.app_id.clone(),
        app_secret: cfg.im.feishu.app_secret.clone(),
        verification_token: cfg.im.feishu.verification_token.clone(),
        encrypt_key: cfg.im.feishu.encrypt_key.clone(),
        webhook_bind: cfg.im.feishu.webhook_bind.clone(),
        allowed_users: cfg.im.feishu.allowed_users.clone(),
    };
    let (channel, inbound_rx, _handle) = FeishuChannel::start(feishu_cfg)
        .await
        .map_err(|e| anyhow!("feishu: {e}"))?;
    Ok((Arc::new(channel), inbound_rx, 60_000))
}

async fn build_telegram(
    cfg: &AppConfig,
) -> Result<(
    Arc<dyn ImChannel>,
    mpsc::Receiver<agentline_bridge::types::InboundMessage>,
    u64,
)> {
    if cfg.im.telegram.bot_token.is_empty() {
        bail!("telegram requires `im.telegram.bot_token` in config");
    }
    let tg_cfg = TelegramConfig {
        bot_token: cfg.im.telegram.bot_token.clone(),
        allowed_users: cfg.im.telegram.allowed_users.clone(),
        api_base: cfg.im.telegram.api_base.clone(),
    };
    let (channel, inbound_rx, _handle) =
        TelegramChannel::start(tg_cfg).map_err(|e| anyhow!("telegram: {e}"))?;
    Ok((Arc::new(channel), inbound_rx, 5_000))
}

fn agent_pid_path(cfg: &AppConfig) -> std::path::PathBuf {
    crate::config::expand_tilde(&cfg.bridge.state_dir).join("agent.pid")
}

async fn build_claude_code(cfg: &AppConfig) -> Result<AcpBackend> {
    let cc = &cfg.agent.claude_code;
    let mut conf = ClaudeCodeConfig::default();
    if !cc.version.is_empty() {
        conf.version = cc.version.clone();
    }
    if !cc.command.is_empty() {
        conf.command = Some(cc.command.clone());
    }
    if !cc.args.is_empty() {
        conf.args = Some(cc.args.clone());
    }
    conf.extra_env = proxy_env(&cfg.proxy);
    conf.extra_env
        .extend(cc.extra_env.iter().map(|(k, v)| (k.clone(), v.clone())));
    conf.remove_env_extra = cc.remove_env_extra.clone();
    conf.inject_settings_env = cc.inject_settings_env;
    conf.pid_file = Some(agent_pid_path(cfg));
    spawn_claude_code(conf)
        .await
        .map_err(|e| anyhow!("spawn claude-code: {e}"))
}

async fn build_kimi(cfg: &AppConfig) -> Result<AcpBackend> {
    let k = &cfg.agent.kimi;
    let mut conf = KimiConfig::default();
    if !k.command.is_empty() {
        conf.command = Some(k.command.clone());
    }
    if !k.args.is_empty() {
        conf.args = Some(k.args.clone());
    }
    conf.extra_env = proxy_env(&cfg.proxy);
    conf.extra_env
        .extend(k.extra_env.iter().map(|(k, v)| (k.clone(), v.clone())));
    if !k.access_token.is_empty() {
        conf.extra_env
            .push(("MOONSHOT_API_KEY".to_string(), k.access_token.clone()));
    }
    conf.remove_env = k.remove_env.clone();
    conf.pid_file = Some(agent_pid_path(cfg));
    spawn_kimi(conf)
        .await
        .map_err(|e| anyhow!("spawn kimi: {e}"))
}

async fn build_qoder(cfg: &AppConfig) -> Result<AcpBackend> {
    let q = &cfg.agent.qoder;
    let mut conf = QoderConfig::default();
    if !q.command.is_empty() {
        conf.command = Some(q.command.clone());
    }
    if !q.args.is_empty() {
        conf.args = Some(q.args.clone());
    }
    conf.extra_env = proxy_env(&cfg.proxy);
    conf.extra_env
        .extend(q.extra_env.iter().map(|(k, v)| (k.clone(), v.clone())));
    conf.remove_env = q.remove_env.clone();
    if !q.personal_access_token.is_empty() {
        conf = conf.with_personal_access_token(q.personal_access_token.clone());
    }
    conf.pid_file = Some(agent_pid_path(cfg));
    spawn_qoder(conf)
        .await
        .map_err(|e| anyhow!("spawn qoder: {e}"))
}

async fn build_opencode(cfg: &AppConfig) -> Result<AcpBackend> {
    let o = &cfg.agent.opencode;
    let mut conf = OpencodeConfig::default();
    if !o.command.is_empty() {
        conf.command = Some(o.command.clone());
    }
    if !o.args.is_empty() {
        conf.args = Some(o.args.clone());
    }
    conf.extra_env = proxy_env(&cfg.proxy);
    conf.extra_env
        .extend(o.extra_env.iter().map(|(k, v)| (k.clone(), v.clone())));
    if !o.api_key.is_empty() {
        conf.extra_env
            .push(("OPENAI_API_KEY".to_string(), o.api_key.clone()));
    }
    if !o.base_url.is_empty() {
        conf.extra_env
            .push(("OPENAI_BASE_URL".to_string(), o.base_url.clone()));
    }
    conf.remove_env = o.remove_env.clone();
    conf.pid_file = Some(agent_pid_path(cfg));
    spawn_opencode(conf)
        .await
        .map_err(|e| anyhow!("spawn opencode: {e}"))
}

async fn build_kiro(cfg: &AppConfig) -> Result<AcpBackend> {
    let k = &cfg.agent.kiro;
    let mut conf = KiroConfig::default();
    if !k.command.is_empty() {
        conf.command = Some(k.command.clone());
    }
    if !k.args.is_empty() {
        conf.args = Some(k.args.clone());
    }
    if !k.agent_name.is_empty() {
        conf.agent_name = Some(k.agent_name.clone());
    }
    conf.extra_env = proxy_env(&cfg.proxy);
    conf.extra_env
        .extend(k.extra_env.iter().map(|(k, v)| (k.clone(), v.clone())));
    conf.remove_env = k.remove_env.clone();
    conf.pid_file = Some(agent_pid_path(cfg));
    spawn_kiro(conf)
        .await
        .map_err(|e| anyhow!("spawn kiro: {e}"))
}

async fn build_gemini(cfg: &AppConfig) -> Result<AcpBackend> {
    let g = &cfg.agent.gemini;
    let mut conf = GeminiConfig::default();
    if !g.command.is_empty() {
        conf.command = Some(g.command.clone());
    }
    if !g.args.is_empty() {
        conf.args = Some(g.args.clone());
    }
    conf.extra_env = proxy_env(&cfg.proxy);
    conf.extra_env
        .extend(g.extra_env.iter().map(|(k, v)| (k.clone(), v.clone())));
    conf.remove_env = g.remove_env.clone();
    conf.pid_file = Some(agent_pid_path(cfg));
    spawn_gemini(conf)
        .await
        .map_err(|e| anyhow!("spawn gemini: {e}"))
}

async fn build_hermes(cfg: &AppConfig) -> Result<AcpBackend> {
    let h = &cfg.agent.hermes;
    let mut conf = HermesConfig::default();
    if !h.command.is_empty() {
        conf.command = Some(h.command.clone());
    }
    if !h.args.is_empty() {
        conf.args = Some(h.args.clone());
    }
    conf.extra_env = proxy_env(&cfg.proxy);
    conf.extra_env
        .extend(h.extra_env.iter().map(|(k, v)| (k.clone(), v.clone())));
    conf.remove_env = h.remove_env.clone();
    conf.pid_file = Some(agent_pid_path(cfg));
    spawn_hermes(conf)
        .await
        .map_err(|e| anyhow!("spawn hermes: {e}"))
}

async fn build_codex(cfg: &AppConfig) -> Result<CodexBackend> {
    let c = &cfg.agent.codex;
    let mut conf = CodexConfig::default();
    if !c.command.is_empty() {
        conf.command = Some(c.command.clone());
    }
    if !c.args.is_empty() {
        conf.args = Some(c.args.clone());
    }
    conf.extra_env = proxy_env(&cfg.proxy);
    conf.extra_env
        .extend(c.extra_env.iter().map(|(k, v)| (k.clone(), v.clone())));
    conf.sandbox_mode = parse_sandbox(&c.sandbox_mode).unwrap_or(conf.sandbox_mode);
    conf.approval_mode = parse_approval(&c.approval_mode).unwrap_or(conf.approval_mode);
    conf.skip_git_repo_check = c.skip_git_repo_check;
    if !c.model.is_empty() {
        conf.model = Some(c.model.clone());
    }
    CodexBackend::spawn(conf)
        .await
        .map_err(|e| anyhow!("spawn codex: {e}"))
}

/// Build the proxy env-var pairs to prepend to every agent's `extra_env`.
///
/// These are injected BEFORE the per-agent `extra_env` so the user can still
/// override individual vars if needed.  `NO_PROXY` always includes the
/// RFC-1918 LAN ranges plus whatever is in `proxy.no_proxy` and the parent
/// process's own `$NO_PROXY`.
fn proxy_env(proxy: &ProxySection) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = Vec::new();

    if !proxy.http.is_empty() {
        pairs.push(("HTTP_PROXY".into(), proxy.http.clone()));
        pairs.push(("http_proxy".into(), proxy.http.clone()));
    }

    let https_val = if !proxy.https.is_empty() {
        proxy.https.clone()
    } else if !proxy.http.is_empty() {
        // fall back to http proxy for HTTPS traffic too
        proxy.http.clone()
    } else {
        String::new()
    };
    if !https_val.is_empty() {
        pairs.push(("HTTPS_PROXY".into(), https_val.clone()));
        pairs.push(("https_proxy".into(), https_val));
    }

    // Always inject NO_PROXY: config entries + parent env + LAN defaults.
    let no_proxy = agentline_bridge::proxy::build_no_proxy_with(&proxy.no_proxy);
    pairs.push(("NO_PROXY".into(), no_proxy.clone()));
    pairs.push(("no_proxy".into(), no_proxy));

    pairs
}

fn parse_sandbox(s: &str) -> Option<SandboxMode> {
    match s {
        "" => None,
        "read-only" => Some(SandboxMode::ReadOnly),
        "workspace-write" => Some(SandboxMode::WorkspaceWrite),
        "danger-full-access" => Some(SandboxMode::DangerFullAccess),
        _ => None,
    }
}

fn parse_approval(s: &str) -> Option<ApprovalMode> {
    match s {
        "" => None,
        "never" => Some(ApprovalMode::Never),
        "on-request" => Some(ApprovalMode::OnRequest),
        "on-failure" => Some(ApprovalMode::OnFailure),
        "untrusted" => Some(ApprovalMode::Untrusted),
        _ => None,
    }
}

async fn build_generic_acp(cfg: &AppConfig) -> Result<AcpBackend> {
    if cfg.agent.acp.command.is_empty() {
        bail!("agent.backend = \"acp\" requires `agent.acp.command` in config");
    }
    let mut extra_env = proxy_env(&cfg.proxy);
    extra_env.extend(
        cfg.agent
            .acp
            .extra_env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );
    let acp_cfg = AcpBackendConfig {
        command: cfg.agent.acp.command.clone(),
        args: cfg.agent.acp.args.clone(),
        extra_env,
        remove_env: cfg.agent.acp.remove_env.clone(),
        pid_file: Some(agent_pid_path(cfg)),
    };
    AcpBackend::spawn(acp_cfg)
        .await
        .map_err(|e| anyhow!("spawn acp: {e}"))
}
