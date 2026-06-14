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

    let event_loop = EventLoopBuilder::new().build();

    #[cfg(target_os = "macos")]
    hide_from_dock();

    let menu = Menu::new();
    let status_item = MenuItem::new(tr.status_checking(), false, None);
    let dashboard = MenuItem::new(tr.open_dashboard(), true, None);
    let restart = MenuItem::new(tr.restart_daemon(), true, None);
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
        &quit,
    ])?;

    let _tray = TrayIconBuilder::new()
        .with_icon(make_icon())
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

    let dashboard_id = dashboard.id().clone();
    let restart_id = restart.id().clone();
    let quit_id = quit.id().clone();

    let child_event = Arc::clone(&child);
    let auto_restart_event = Arc::clone(&auto_restart);
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(
            std::time::Instant::now() + Duration::from_millis(POLL_INTERVAL_MS / 2),
        );

        if matches!(event, Event::NewEvents(_) | Event::MainEventsCleared) {
            while let Ok((state, pid)) = state_rx.try_recv() {
                status_item.set_text(state.label(&tr, pid));
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

// ─── macOS: hide dock icon ───────────────────────────────────────

#[cfg(target_os = "macos")]
fn hide_from_dock() {
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::MainThreadMarker;

    if let Some(mtm) = MainThreadMarker::new() {
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    } else {
        tracing::warn!("not on main thread; dock icon may be visible");
    }
}

// ─── helpers ────────────────────────────────────────────────────

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Rasterize the embedded SVG to a fixed-size RGBA pixmap and hand it
/// to tray-icon. Rendered at 2× source size (64×64) so the menu bar
/// looks sharp on retina displays — macOS scales it back down as needed.
fn make_icon() -> tray_icon::Icon {
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

    tray_icon::Icon::from_rgba(pixmap.take(), SIZE, SIZE).expect("build icon")
}
