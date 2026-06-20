//! claude-code native login detection (self-contained in the claude-code crate).
//!
//! Logged in via any of: an injected `ANTHROPIC_*` key, the `env` block of
//! `~/.claude/settings.json`, the macOS keychain, or the Linux/Windows
//! `~/.claude/.credentials.json`.

/// `extra_env` is the agent's injected env (from agentline's own config).
pub fn is_logged_in(extra_env: &[(String, String)]) -> bool {
    extra_env
        .iter()
        .any(|(k, v)| !v.is_empty() && (k == "ANTHROPIC_API_KEY" || k == "ANTHROPIC_AUTH_TOKEN"))
        || settings_env_has_key()
        || keychain_has("Claude Code-credentials")
        || home_file_exists(&[".claude", ".credentials.json"])
}

fn home_file_exists(segments: &[&str]) -> bool {
    let Some(mut p) = dirs::home_dir() else {
        return false;
    };
    for s in segments {
        p.push(s);
    }
    p.exists()
}

fn settings_env_has_key() -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let path = home.join(".claude").join("settings.json");
    let Ok(data) = std::fs::read(&path) else {
        return false;
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&data) else {
        return false;
    };
    let Some(env) = v.get("env").and_then(|e| e.as_object()) else {
        return false;
    };
    env.get("ANTHROPIC_API_KEY")
        .or_else(|| env.get("ANTHROPIC_AUTH_TOKEN"))
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
}

fn keychain_has(service: &str) -> bool {
    std::process::Command::new("security")
        .args(["find-generic-password", "-s", service])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()
        .is_some_and(|s| s.success())
}
