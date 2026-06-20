//! Inherit env vars from `~/.claude/settings.json`.
//!
//! Claude Code's CLI puts proxy/auth settings (e.g. `ANTHROPIC_BASE_URL`,
//! `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_MODEL`) in `~/.claude/settings.json`'s
//! top-level `env` block. The CLI applies them when it runs, but a freshly
//! spawned `npx @zed-industries/claude-code-acp` child won't see them unless
//! we inject them ourselves.
//!
//! Called automatically by [`spawn`](crate::spawn) when
//! `ClaudeCodeConfig::inject_settings_env = true` (default).

use std::path::PathBuf;

use agentline_bridge::{Error, Result};

pub fn settings_json_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".claude/settings.json"))
}

/// Read `~/.claude/settings.json` and `set_var` every string value under `env`.
///
/// Returns the number of variables injected. If the file is missing, returns
/// `Ok(0)` — most users running `claude /login` instead of using a proxy
/// won't have these.
///
/// **Note:** mutates the process environment. Only call from a context where
/// that's acceptable (program startup).
pub fn inject_claude_settings_env() -> Result<usize> {
    let Some(path) = settings_json_path() else {
        return Ok(0);
    };
    if !path.exists() {
        return Ok(0);
    }
    let bytes = std::fs::read(&path).map_err(|e| Error::other(format!("read {path:?}: {e}")))?;
    let val: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| Error::other(format!("parse {path:?}: {e}")))?;
    let Some(env) = val.get("env").and_then(|v| v.as_object()) else {
        return Ok(0);
    };
    let mut count = 0;
    for (k, v) in env {
        let Some(s) = v.as_str() else { continue };
        if std::env::var_os(k).is_some() {
            continue;
        }
        // SAFETY: we are deliberately mutating the process environment at startup.
        unsafe {
            std::env::set_var(k, s);
        }
        count += 1;
    }
    Ok(count)
}
