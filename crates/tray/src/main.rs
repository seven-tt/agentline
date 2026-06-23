use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentline_bridge::process;
use anyhow::{Context, Result, bail};
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIconBuilder, TrayIconEvent};

const POLL_INTERVAL_MS: u64 = 2_000;

type ChildHandle = Arc<Mutex<Option<Child>>>;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    run_tray()
}

// ─── i18n ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Locale {
    ZhCN,
    En,
}

impl Locale {
    fn from_config() -> Self {
        let path = home().join(".agentline/config.toml");
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("locale") {
                if line.contains("\"en\"") || line.contains("'en'") {
                    return Locale::En;
                }
                return Locale::ZhCN;
            }
        }
        Locale::ZhCN
    }
}

struct Tr {
    locale: Locale,
}

impl Tr {
    fn new(locale: Locale) -> Self {
        Self { locale }
    }

    fn status_checking(&self) -> &str {
        match self.locale {
            Locale::ZhCN => "状态：检查中…",
            Locale::En => "Status: checking…",
        }
    }

    fn open_dashboard(&self) -> &str {
        match self.locale {
            Locale::ZhCN => "打开面板",
            Locale::En => "Open dashboard",
        }
    }

    fn restart_daemon(&self) -> &str {
        match self.locale {
            Locale::ZhCN => "重启服务",
            Locale::En => "Restart daemon",
        }
    }

    fn quit(&self) -> &str {
        match self.locale {
            Locale::ZhCN => "退出 Agentline",
            Locale::En => "Quit Agentline",
        }
    }

    fn status_running(&self, pid: Option<u32>) -> String {
        match self.locale {
            Locale::ZhCN => match pid {
                Some(p) => format!("状态：● 运行中   pid={p}"),
                None => "状态：● 运行中".into(),
            },
            Locale::En => match pid {
                Some(p) => format!("Status: ● running   pid={p}"),
                None => "Status: ● running".into(),
            },
        }
    }

    fn status_not_running(&self) -> &str {
        match self.locale {
            Locale::ZhCN => "状态：✕ 未运行",
            Locale::En => "Status: ✕ not running",
        }
    }

    fn check_update(&self) -> &str {
        match self.locale {
            Locale::ZhCN => "检查更新",
            Locale::En => "Check for updates",
        }
    }

    fn check_update_available(&self) -> &str {
        match self.locale {
            Locale::ZhCN => "检查更新 🔴",
            Locale::En => "Check for updates 🔴",
        }
    }

    fn downloading(&self, pct: u8) -> String {
        match self.locale {
            Locale::ZhCN => format!("下载中 {pct}%"),
            Locale::En => format!("Downloading {pct}%"),
        }
    }

    fn installing(&self) -> &str {
        match self.locale {
            Locale::ZhCN => "安装中...",
            Locale::En => "Installing...",
        }
    }
}

// ─── tray app ───────────────────────────────────────────────────

fn run_tray() -> Result<()> {
    let locale = Locale::from_config();
    let tr = Tr::new(locale);

    let child: ChildHandle = Arc::new(Mutex::new(None));

    // Kill any leftover daemon/agent from a previous crash before spawning.
    kill_stale_daemon();
    cleanup_orphaned_agent();
    match spawn_daemon() {
        Ok(c) => {
            tracing::info!(pid = c.id(), "daemon started");
            *child.lock().unwrap() = Some(c);
        }
        Err(e) => tracing::warn!(error=%e, "could not start daemon"),
    }

    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut event_loop = EventLoopBuilder::new().build();

    // Must be set on tao's own EventLoop before `run()` — tao resets the
    // activation policy to Regular when `applicationDidFinishLaunching`
    // fires, which clobbers any policy set via AppKit directly beforehand.
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
    }

    let menu = Menu::new();
    let status_item = MenuItem::new(tr.status_checking(), false, None);
    let dashboard = MenuItem::new(tr.open_dashboard(), true, None);
    let restart = MenuItem::new(tr.restart_daemon(), true, None);
    let update_item = MenuItem::new(tr.check_update(), false, None);
    let sep1 = PredefinedMenuItem::separator();
    let sep2 = PredefinedMenuItem::separator();
    let sep3 = PredefinedMenuItem::separator();
    let quit = MenuItem::new(tr.quit(), true, None);
    menu.append_items(&[
        &status_item,
        &sep1,
        &dashboard,
        &sep2,
        &restart,
        &sep3,
        &update_item,
        &quit,
    ])?;

    let tray = TrayIconBuilder::new()
        .with_icon(make_icon(false))
        .with_icon_as_template(true)
        .with_tooltip("Agentline")
        .with_menu(Box::new(menu))
        .build()?;

    let menu_rx = MenuEvent::receiver();
    let tray_rx = TrayIconEvent::receiver();

    let auto_restart = Arc::new(AtomicBool::new(true));

    let poll_child = Arc::clone(&child);
    let poll_restart = Arc::clone(&auto_restart);
    let (state_tx, state_rx) = std::sync::mpsc::channel::<(DaemonState, Option<u32>)>();
    std::thread::spawn(move || {
        loop {
            let _ = state_tx.send(check_child_and_maybe_restart(&poll_child, &poll_restart));
            std::thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
        }
    });

    // Update checker: sends UpdateMsg to the event loop
    let (update_tx, update_rx) = std::sync::mpsc::channel::<UpdateMsg>();
    let update_tx_bg = update_tx.clone();
    std::thread::spawn(move || {
        loop {
            if let Some(info) = check_github_release() {
                let _ = update_tx_bg.send(UpdateMsg::Available(info));
            }
            std::thread::sleep(Duration::from_secs(30 * 60));
        }
    });

    let dashboard_id = dashboard.id().clone();
    let restart_id = restart.id().clone();
    let update_id = update_item.id().clone();
    let quit_id = quit.id().clone();

    let child_event = Arc::clone(&child);
    let auto_restart_event = Arc::clone(&auto_restart);
    let mut pending_release: Option<ReleaseInfo> = None;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(
            std::time::Instant::now() + Duration::from_millis(POLL_INTERVAL_MS / 2),
        );

        if matches!(event, Event::NewEvents(_) | Event::MainEventsCleared) {
            while let Ok((state, pid)) = state_rx.try_recv() {
                status_item.set_text(state.label(&tr, pid));
            }
            while let Ok(msg) = update_rx.try_recv() {
                match msg {
                    UpdateMsg::Available(info) => {
                        update_item.set_text(tr.check_update_available());
                        update_item.set_enabled(true);
                        let _ = tray.set_icon_with_as_template(Some(make_icon(true)), false);
                        pending_release = Some(info);
                    }
                    UpdateMsg::Progress(pct) => {
                        update_item.set_text(tr.downloading(pct));
                        update_item.set_enabled(false);
                    }
                    UpdateMsg::Installing => {
                        update_item.set_text(tr.installing());
                    }
                    UpdateMsg::Done => {
                        do_self_replace_and_restart(&child_event, &auto_restart_event);
                    }
                    UpdateMsg::Failed => {
                        update_item.set_text(tr.check_update_available());
                        update_item.set_enabled(true);
                    }
                }
            }
            while let Ok(ev) = menu_rx.try_recv() {
                if ev.id == quit_id {
                    auto_restart_event.store(false, Ordering::Relaxed);
                    kill_daemon(&child_event);
                    kill_stale_daemon();
                    cleanup_orphaned_agent();
                    *control_flow = ControlFlow::Exit;
                } else if ev.id == restart_id {
                    if let Err(e) = do_restart(&child_event) {
                        tracing::warn!(error=%e, "restart failed");
                    }
                } else if ev.id == dashboard_id {
                    open_url("http://127.0.0.1:7681");
                } else if ev.id == update_id
                    && let Some(info) = pending_release.clone()
                {
                    update_item.set_text(tr.downloading(0));
                    update_item.set_enabled(false);
                    let tx = update_tx.clone();
                    std::thread::spawn(move || download_and_install(info, tx));
                }
            }
            while let Ok(_e) = tray_rx.try_recv() {}
        }
    });
}

/// Open a URL in the default browser (cross-platform).
fn open_url(url: &str) {
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg(url).status();
    #[cfg(windows)]
    let _ = Command::new("cmd").args(["/c", "start", url]).status();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = Command::new("xdg-open").arg(url).status();
}

// ─── orphan cleanup ──────────────────────────────────────────────

/// Kill any orphaned ACP agent process whose PID was recorded by the daemon.
/// Called before every spawn so a SIGKILL-ed daemon doesn't leave the agent
/// tree alive.
fn cleanup_orphaned_agent() {
    let pid_file = home().join(".agentline/agent.pid");
    let content = match std::fs::read_to_string(&pid_file) {
        Ok(c) => c,
        Err(_) => return,
    };
    let pid: i32 = match content.trim().parse() {
        Ok(p) if p > 1 => p,
        _ => {
            let _ = std::fs::remove_file(&pid_file);
            return;
        }
    };
    if !process::process_is_alive(pid) {
        let _ = std::fs::remove_file(&pid_file);
        return;
    }
    tracing::info!(pid, "killing orphaned agent process tree");
    #[cfg(target_os = "macos")]
    kill_agent_session(pid);
    process::kill_process_group(pid);
    let _ = std::fs::remove_file(&pid_file);
}

/// Kill every process belonging to session `sid` (macOS only).
/// The ACP child runs under `setsid`, so pid == sid. `npm exec` escapes the
/// process group via `setpgid`, but cannot escape the session.
#[cfg(target_os = "macos")]
fn kill_agent_session(sid: i32) {
    const PROC_ALL_PIDS: u32 = 1;
    unsafe {
        let cap = libc::proc_listpids(PROC_ALL_PIDS, 0, std::ptr::null_mut(), 0);
        if cap <= 0 {
            return;
        }
        let mut pids = vec![0i32; cap as usize / std::mem::size_of::<i32>() + 16];
        let bytes = libc::proc_listpids(
            PROC_ALL_PIDS,
            0,
            pids.as_mut_ptr() as *mut libc::c_void,
            (pids.len() * std::mem::size_of::<i32>()) as i32,
        );
        if bytes <= 0 {
            return;
        }
        let n = bytes as usize / std::mem::size_of::<i32>();
        for &p in &pids[..n] {
            if p <= 1 {
                continue;
            }
            if libc::getsid(p) == sid {
                libc::kill(p, libc::SIGTERM);
                libc::kill(p, libc::SIGKILL);
            }
        }
    }
}

// ─── stale daemon cleanup ────────────────────────────────────────

/// Kill any daemon process whose PID was recorded in the lock file.
/// This handles the case where the tray was relaunched (or crashed) while
/// the old daemon is still alive — `kill_daemon()` can't reach it because
/// the Child handle was lost.
fn kill_stale_daemon() {
    let lock_file = home().join(".agentline/agentline.lock");
    let content = match std::fs::read_to_string(&lock_file) {
        Ok(c) => c,
        Err(_) => return,
    };
    let pid: i32 = match content.trim().parse() {
        Ok(p) if p > 1 => p,
        _ => return,
    };
    if !process::process_is_alive(pid) {
        return;
    }
    tracing::info!(pid, "killing stale daemon from lock file");
    #[cfg(unix)]
    {
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            std::thread::sleep(Duration::from_millis(100));
            if unsafe { libc::kill(pid, 0) } != 0 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                tracing::warn!(
                    pid,
                    "stale daemon did not exit after SIGTERM, sending SIGKILL"
                );
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
                std::thread::sleep(Duration::from_millis(200));
                break;
            }
        }
    }
    #[cfg(windows)]
    {
        process::kill_single_process(pid as u32);
        std::thread::sleep(Duration::from_millis(200));
    }
}

// ─── daemon lifecycle ────────────────────────────────────────────

fn agentline_bin() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("current_exe")?;
    let dir = exe.parent().context("no parent dir")?;
    let name = if cfg!(windows) {
        "agentline.exe"
    } else {
        "agentline"
    };
    let bin = dir.join(name);
    if !bin.exists() {
        bail!("agentline not found at {}", bin.display());
    }
    Ok(bin)
}

fn spawn_daemon() -> Result<Child> {
    let bin = agentline_bin()?;
    let log_path = home().join(".agentline/agentline.log");
    if let Some(p) = log_path.parent() {
        std::fs::create_dir_all(p).ok();
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .context("open daemon log")?;
    let log2 = log.try_clone().context("clone log fd")?;
    Command::new(&bin)
        .arg("run")
        .env("PATH", enriched_path())
        // Log level is driven by config (`[log] level`); don't force RUST_LOG
        // here or it would override the user's setting.
        .stdout(log)
        .stderr(log2)
        .spawn()
        .with_context(|| format!("spawn {}", bin.display()))
}

/// Get the user's real PATH by sourcing their login shell profile.
/// Falls back to a manually constructed path if the shell call fails.
#[cfg(unix)]
fn enriched_path() -> String {
    // Ask an interactive login shell — reads .zprofile AND .zshrc so user
    // customizations (NVM, kimi, etc.) are included.
    for args in &[
        vec!["-i", "-l", "-c", "echo $PATH"],
        vec!["-l", "-c", "echo $PATH"],
    ] {
        for shell in &["/bin/zsh", "/bin/bash"] {
            if let Ok(out) = Command::new(shell).args(args).env("TERM", "dumb").output()
                && out.status.success()
            {
                let p = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .find(|l| l.contains('/'))
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !p.is_empty() {
                    return p;
                }
            }
        }
    }

    // Fallback: manually add common locations.
    let mut paths: Vec<String> = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    for c in &[
        "/opt/homebrew/bin",
        "/opt/homebrew/sbin",
        "/usr/local/bin",
        "/usr/local/sbin",
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
    ] {
        let s = c.to_string();
        if std::path::Path::new(c).exists() && !paths.contains(&s) {
            paths.insert(0, s);
        }
    }
    paths.join(":")
}

#[cfg(windows)]
fn enriched_path() -> String {
    std::env::var("PATH").unwrap_or_default()
}

fn kill_daemon(handle: &ChildHandle) {
    if let Some(mut c) = handle.lock().unwrap().take() {
        // SIGTERM first so the daemon runs its shutdown handler and kills its
        // own agent process tree (npx → node → claude). A bare SIGKILL would
        // orphan that tree. Fall back to SIGKILL if it doesn't exit promptly.
        #[cfg(unix)]
        unsafe {
            libc::kill(c.id() as i32, libc::SIGTERM);
        }
        #[cfg(windows)]
        process::kill_single_process(c.id());
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match c.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                _ => break,
            }
        }
        let _ = c.kill();
        let _ = c.wait();
    }
}

fn do_restart(handle: &ChildHandle) -> Result<()> {
    kill_daemon(handle);
    kill_stale_daemon();
    cleanup_orphaned_agent();
    let c = spawn_daemon()?;
    tracing::info!(pid = c.id(), "daemon restarted");
    *handle.lock().unwrap() = Some(c);
    Ok(())
}

// ─── daemon state ────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonState {
    Running,
    NotRunning,
}

impl DaemonState {
    fn label(self, tr: &Tr, pid: Option<u32>) -> String {
        match self {
            DaemonState::Running => tr.status_running(pid),
            DaemonState::NotRunning => tr.status_not_running().into(),
        }
    }
}

fn check_child_and_maybe_restart(
    handle: &ChildHandle,
    auto_restart: &Arc<AtomicBool>,
) -> (DaemonState, Option<u32>) {
    let exited = {
        let mut guard = handle.lock().unwrap();
        match guard.as_mut() {
            None => true,
            Some(c) => match c.try_wait() {
                Ok(None) => return (DaemonState::Running, Some(c.id())),
                // Only treat a *confirmed* exit as reason to respawn. A transient
                // try_wait() error must NOT trigger a respawn — doing so spawns a
                // second daemon while the first is still alive (double replies).
                Ok(Some(_)) => {
                    *guard = None;
                    true
                }
                Err(_) => return (DaemonState::Running, Some(c.id())),
            },
        }
    };
    if exited && auto_restart.load(Ordering::Relaxed) {
        kill_stale_daemon();
        cleanup_orphaned_agent();
        match spawn_daemon() {
            Ok(c) => {
                tracing::info!(pid = c.id(), "daemon auto-restarted");
                *handle.lock().unwrap() = Some(c);
                return (DaemonState::Running, None);
            }
            Err(e) => tracing::warn!(error=%e, "auto-restart failed"),
        }
    }
    (DaemonState::NotRunning, None)
}

// ─── helpers ────────────────────────────────────────────────────

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Best-effort check of system-wide dark mode, used to recolor the
/// non-template badge icon to match the menu bar.
fn is_dark_menu_bar() -> bool {
    Command::new("defaults")
        .args(["read", "-g", "AppleInterfaceStyle"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "Dark")
        .unwrap_or(false)
}

/// Rasterize the embedded SVG to a fixed-size RGBA pixmap and hand it
/// to tray-icon. Rendered at 2× source size (64×64) so the menu bar
/// looks sharp on retina displays — macOS scales it back down as needed.
///
/// `with_badge` overlays a solid red dot in the top-right corner to flag
/// an available update. Drawn in full color, so callers must pair it with
/// `set_icon_with_as_template(_, false)` — template-image mode would
/// otherwise flatten the dot to the menu bar's monochrome tint.
fn make_icon(with_badge: bool) -> tray_icon::Icon {
    const SVG: &[u8] = include_bytes!("../assets/icon.svg");
    const SIZE: u32 = 64;

    let tree = resvg::usvg::Tree::from_data(SVG, &resvg::usvg::Options::default())
        .expect("parse icon.svg");
    let svg_size = tree.size();
    let sx = SIZE as f32 / svg_size.width();
    let sy = SIZE as f32 / svg_size.height();

    let mut pixmap = resvg::tiny_skia::Pixmap::new(SIZE, SIZE).expect("alloc pixmap");
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(sx, sy),
        &mut pixmap.as_mut(),
    );

    if with_badge {
        // The SVG is "black on transparent" by design, meant to be auto-tinted
        // by `with_icon_as_template`. We can't use template mode here (it would
        // also flatten the red dot to monochrome), so on a dark menu bar we
        // manually recolor the glyph to white. Pixels are premultiplied RGBA8;
        // since alpha is unchanged, white-premultiplied == (a, a, a, a).
        if is_dark_menu_bar() {
            for px in pixmap.pixels_mut() {
                let a = px.alpha();
                *px = resvg::tiny_skia::PremultipliedColorU8::from_rgba(a, a, a, a)
                    .expect("valid premultiplied color");
            }
        }

        let mut pb = resvg::tiny_skia::PathBuilder::new();
        pb.push_circle(55.0, 9.0, 7.0);
        if let Some(path) = pb.finish() {
            let mut paint = resvg::tiny_skia::Paint::default();
            paint.set_color_rgba8(255, 59, 48, 255);
            paint.anti_alias = true;
            pixmap.fill_path(
                &path,
                &paint,
                resvg::tiny_skia::FillRule::Winding,
                resvg::tiny_skia::Transform::identity(),
                None,
            );
        }
    }

    tray_icon::Icon::from_rgba(pixmap.take(), SIZE, SIZE).expect("build icon")
}

// ─── auto update ────────────────────────────────────────────────

const GITHUB_RELEASES_API: &str = "https://api.github.com/repos/seven-tt/agentline/releases/latest";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone)]
struct ReleaseInfo {
    tag: String,
    asset_url: String,
}

enum UpdateMsg {
    Available(ReleaseInfo),
    Progress(u8),
    Installing,
    Done,
    Failed,
}

fn apply_proxy_from_config() {
    let path = home().join(".agentline/config.toml");
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let mut in_proxy = false;
    let mut cfg_http = String::new();
    let mut cfg_https = String::new();
    let mut cfg_no_proxy = String::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_proxy = trimmed == "[proxy]";
            continue;
        }
        if !in_proxy {
            continue;
        }
        if let Some((key, val)) = trimmed.split_once('=') {
            let key = key.trim();
            let val = val.trim().trim_matches('"').to_string();
            match key {
                "http" => cfg_http = val,
                "https" => cfg_https = val,
                "no_proxy" => cfg_no_proxy = val,
                _ => {}
            }
        }
    }

    // Empty config field = fall back to whatever the user already has set in
    // their shell (env or .zshrc/.bashrc via detect_shell_proxy), so the tray
    // matches the user's ambient proxy setup instead of silently going direct.
    let shell = agentline_bridge::proxy::detect_shell_proxy();
    let http_val = if !cfg_http.is_empty() {
        cfg_http
    } else {
        shell.http.clone()
    };
    let https_val = if !cfg_https.is_empty() {
        cfg_https
    } else if !http_val.is_empty() {
        http_val.clone()
    } else if !shell.https.is_empty() {
        shell.https.clone()
    } else {
        shell.http.clone()
    };
    let no_proxy_val = agentline_bridge::proxy::build_no_proxy_with(&cfg_no_proxy);

    // Safety: called from a single background thread before any concurrent access.
    unsafe {
        if !http_val.is_empty() {
            std::env::set_var("http_proxy", &http_val);
            std::env::set_var("HTTP_PROXY", &http_val);
        }
        if !https_val.is_empty() {
            std::env::set_var("https_proxy", &https_val);
            std::env::set_var("HTTPS_PROXY", &https_val);
        }
        std::env::set_var("no_proxy", &no_proxy_val);
        std::env::set_var("NO_PROXY", &no_proxy_val);
    }
}

fn check_github_release() -> Option<ReleaseInfo> {
    apply_proxy_from_config();

    let body: String = ureq::get(GITHUB_RELEASES_API)
        .header("User-Agent", "agentline-tray")
        .header("Accept", "application/vnd.github+json")
        .call()
        .ok()?
        .body_mut()
        .read_to_string()
        .ok()?;

    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    let tag = json.get("tag_name")?.as_str()?;

    if !is_newer(tag, CURRENT_VERSION) {
        return None;
    }

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

    Some(ReleaseInfo {
        tag: tag.to_string(),
        asset_url,
    })
}

fn is_newer(remote_tag: &str, local: &str) -> bool {
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

fn download_and_install(info: ReleaseInfo, tx: std::sync::mpsc::Sender<UpdateMsg>) {
    tracing::info!(tag=%info.tag, "starting update download");
    if let Err(e) = do_download_and_install(&info, &tx) {
        tracing::warn!(error=%e, "update failed");
        let _ = tx.send(UpdateMsg::Failed);
    }
}

fn do_download_and_install(
    info: &ReleaseInfo,
    tx: &std::sync::mpsc::Sender<UpdateMsg>,
) -> Result<()> {
    let tmp_dmg = std::path::Path::new("/tmp/agentline-update.dmg");
    let tmp_dir = std::path::Path::new("/tmp/agentline-update");
    let tmp_mount = std::path::Path::new("/tmp/agentline-mount");

    let _ = std::fs::remove_file(tmp_dmg);
    let _ = std::fs::remove_dir_all(tmp_dir);
    let _ = Command::new("hdiutil")
        .args(["detach", "/tmp/agentline-mount", "-quiet", "-force"])
        .status();

    // Download with progress
    let mut resp = ureq::get(&info.asset_url)
        .header("User-Agent", "agentline-tray")
        .call()
        .context("download request")?;

    let content_len = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    let mut file = std::fs::File::create(tmp_dmg).context("create tmp dmg")?;
    let mut downloaded: u64 = 0;
    let mut last_pct: u8 = 0;
    let mut buf = [0u8; 65536];
    let body = resp.body_mut();
    loop {
        let n = body.as_reader().read(&mut buf).context("read body")?;
        if n == 0 {
            break;
        }
        std::io::Write::write_all(&mut file, &buf[..n])?;
        downloaded += n as u64;
        if let Some(pct_val) = (downloaded * 100).checked_div(content_len) {
            let pct = pct_val.min(100) as u8;
            if pct != last_pct {
                last_pct = pct;
                let _ = tx.send(UpdateMsg::Progress(pct));
            }
        }
    }
    drop(file);

    let _ = tx.send(UpdateMsg::Installing);

    // Mount DMG
    let status = Command::new("hdiutil")
        .args([
            "attach",
            tmp_dmg.to_str().unwrap(),
            "-nobrowse",
            "-quiet",
            "-mountpoint",
            tmp_mount.to_str().unwrap(),
        ])
        .status()
        .context("hdiutil attach")?;
    if !status.success() {
        bail!("hdiutil attach failed");
    }

    // Copy .app from mounted DMG
    std::fs::create_dir_all(tmp_dir)?;
    let status = Command::new("cp")
        .args([
            "-R",
            tmp_mount
                .join("AgentlineTray.app")
                .to_str()
                .unwrap_or_default(),
            tmp_dir.to_str().unwrap(),
        ])
        .status()
        .context("cp from dmg")?;

    // Always detach
    let _ = Command::new("hdiutil")
        .args(["detach", tmp_mount.to_str().unwrap(), "-quiet"])
        .status();

    if !status.success() {
        bail!("cp from DMG failed");
    }

    let extracted_app = tmp_dir.join("AgentlineTray.app");
    if !extracted_app.exists() {
        bail!("AgentlineTray.app not found in DMG");
    }

    // Determine current .app location
    let current_exe = std::env::current_exe().context("current_exe")?;
    let current_app = current_exe
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .context("cannot determine .app path")?;

    let app_parent = current_app.parent().context("no parent of .app")?;
    let app_name = current_app
        .file_name()
        .context("no .app filename")?
        .to_owned();

    let backup = app_parent.join("AgentlineTray.app.bak");
    let _ = std::fs::remove_dir_all(&backup);
    std::fs::rename(current_app, &backup).context("move old .app to backup")?;
    if let Err(e) = std::fs::rename(&extracted_app, app_parent.join(&app_name)) {
        let _ = std::fs::rename(&backup, current_app);
        return Err(e).context("move new .app into place");
    }
    let _ = std::fs::remove_dir_all(&backup);
    let _ = std::fs::remove_file(tmp_dmg);
    let _ = std::fs::remove_dir_all(tmp_dir);

    let _ = tx.send(UpdateMsg::Done);
    Ok(())
}

fn do_self_replace_and_restart(child_handle: &ChildHandle, auto_restart: &Arc<AtomicBool>) {
    auto_restart.store(false, Ordering::Relaxed);
    kill_daemon(child_handle);

    let exe = std::env::current_exe().unwrap_or_default();
    // The new binary is now at the same path (replaced .app)
    let _ = Command::new(&exe).spawn();
    std::process::exit(0);
}
