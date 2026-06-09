//! macOS launchd integration for the `agentline service ...` subcommands.
//!
//! Generates `~/Library/LaunchAgents/com.agentline.daemon.plist` and
//! shells out to `launchctl` for install / uninstall / status. Linux /
//! Windows are not yet implemented; the subcommand will return a clear
//! error there.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const LABEL: &str = "com.agentline.daemon";

fn home() -> Result<PathBuf> {
    dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not determine home dir"))
}

#[cfg(unix)]
fn current_uid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: getuid is a pure read of the process's real user id.
    unsafe { getuid() }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

pub fn plist_path() -> Result<PathBuf> {
    Ok(home()?
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

pub fn log_path() -> Result<PathBuf> {
    Ok(home()?.join(".agentline/agentline.log"))
}

fn render_plist(exe: &Path, config: Option<&Path>, log: &Path, home: &Path) -> String {
    let mut args = vec!["run".to_string()];
    if let Some(c) = config {
        args.push("--config".into());
        args.push(c.display().to_string());
    }
    let program_args_xml: String = std::iter::once(exe.display().to_string())
        .chain(args)
        .map(|a| format!("        <string>{}</string>", xml_escape(&a)))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
{args_xml}
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
    <key>WorkingDirectory</key>
    <string>{home}</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
    </dict>
</dict>
</plist>
"#,
        label = LABEL,
        args_xml = program_args_xml,
        log = xml_escape(&log.display().to_string()),
        home = xml_escape(&home.display().to_string()),
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Running,
    LoadedButNotRunning,
    NotInstalled,
}

pub fn status() -> Result<(ServiceState, Option<i32>, Option<String>)> {
    macos_only()?;
    let domain = format!("gui/{}/{LABEL}", current_uid());
    let out = Command::new("launchctl")
        .args(["print", &domain])
        .output()
        .context("launchctl print")?;
    if !out.status.success() {
        return Ok((ServiceState::NotInstalled, None, None));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let pid = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("pid = "))
        .and_then(|s| s.trim().parse::<i32>().ok());
    let last_exit = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("last exit code = "))
        .map(|s| s.trim().to_string());
    let state = if pid.is_some() {
        ServiceState::Running
    } else {
        ServiceState::LoadedButNotRunning
    };
    Ok((state, pid, last_exit))
}

pub fn install(config: Option<&Path>) -> Result<()> {
    macos_only()?;
    let exe = std::env::current_exe().context("current_exe")?;
    let home = home()?;
    let log = log_path()?;
    let plist = plist_path()?;

    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    if let Some(parent) = plist.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }

    let content = render_plist(&exe, config, &log, &home);
    std::fs::write(&plist, content).with_context(|| format!("write {}", plist.display()))?;
    eprintln!("→ wrote {}", plist.display());

    // bootstrap into user gui domain
    let domain = format!("gui/{}", current_uid());
    // If already loaded, bootstrap will fail — bootout first, idempotent.
    let _ = Command::new("launchctl")
        .args(["bootout", &format!("{domain}/{LABEL}")])
        .status();
    let out = Command::new("launchctl")
        .args(["bootstrap", &domain, plist.to_str().unwrap()])
        .output()
        .context("launchctl bootstrap")?;
    if !out.status.success() {
        bail!(
            "launchctl bootstrap failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    // Kickstart immediately so the user doesn't have to wait for next login.
    let _ = Command::new("launchctl")
        .args(["kickstart", "-k", &format!("{domain}/{LABEL}")])
        .status();

    eprintln!("→ launchctl bootstrap {domain} {LABEL}");
    eprintln!("✅ installed. logs: {}", log.display());
    Ok(())
}

pub fn uninstall() -> Result<()> {
    macos_only()?;
    let plist = plist_path()?;
    let domain_target = format!("gui/{}/{LABEL}", current_uid());
    let _ = Command::new("launchctl")
        .args(["bootout", &domain_target])
        .status();
    if plist.exists() {
        std::fs::remove_file(&plist).with_context(|| format!("remove {}", plist.display()))?;
        eprintln!("→ removed {}", plist.display());
    }
    eprintln!("✅ uninstalled");
    Ok(())
}

pub fn print_status() -> Result<()> {
    let (state, pid, last_exit) = status()?;
    match state {
        ServiceState::Running => println!(
            "running  pid={}  log={}",
            pid.unwrap_or(0),
            log_path()?.display()
        ),
        ServiceState::LoadedButNotRunning => println!(
            "loaded but not running  last_exit={}  log={}",
            last_exit.as_deref().unwrap_or("?"),
            log_path()?.display()
        ),
        ServiceState::NotInstalled => println!("not installed (run `agentline service install`)"),
    }
    Ok(())
}

pub fn show_logs(tail: bool) -> Result<()> {
    let log = log_path()?;
    if !log.exists() {
        println!("(no log file yet at {})", log.display());
        return Ok(());
    }
    let mut cmd = if tail {
        let mut c = Command::new("tail");
        c.args(["-n", "200", "-f"]);
        c
    } else {
        let mut c = Command::new("tail");
        c.args(["-n", "200"]);
        c
    };
    cmd.arg(&log);
    let status = cmd.status().context("tail")?;
    if !status.success() {
        bail!("tail exited {}", status);
    }
    Ok(())
}

fn macos_only() -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!(
            "agentline service is only implemented for macOS right now. \
             On Windows, run `agentline run` directly or use Task Scheduler."
        );
    }
    Ok(())
}
