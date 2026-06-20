//! kimi's native config / credential management (self-contained in the kimi
//! crate, not the generic CLI layer).
//!
//! - login detection: the device-code credential file written by `kimi login`
//! - API-key persistence: `~/.kimi-code/config.toml` (format-preserving)
//! - first-run config seeding

use std::path::Path;

/// The minimal but *complete* kimi config written when none exists yet. Kept as
/// a const so the daemon and tests use the exact same template — a bare
/// `api_key` on its own would make kimi fail to start.
pub const CONFIG_TEMPLATE: &str = r#"default_model = "kimi-code/kimi-for-coding"
default_thinking = true

[providers."managed:kimi-code"]
type = "kimi"
base_url = "https://api.kimi.com/coding/v1"
api_key = ""

[models."kimi-code/kimi-for-coding"]
provider = "managed:kimi-code"
model = "kimi-for-coding"
max_context_size = 262144
"#;

/// Whether kimi is logged in: a non-expired device-code credential exists.
///
/// kimi `acp` authenticates ONLY via the `kimi login` device-code credential —
/// the provider api_key in config.toml is NOT accepted by the ACP server (see
/// [`check_config_provider_key`], kept but unused for now).
pub fn is_logged_in() -> bool {
    credential_valid(&[".kimi-code", "credentials", "kimi-code.json"])
        || credential_valid(&[".kimi", "credentials", "kimi-code.json"])
}

/// Seed a complete default config on first install when none exists.
pub fn post_install() {
    let Some(home) = dirs::home_dir() else { return };
    let dir = home.join(".kimi-code");
    let path = dir.join("config.toml");
    if path.exists()
        && let Ok(content) = std::fs::read_to_string(&path)
        && content.contains("[providers.")
    {
        return;
    }
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&path, CONFIG_TEMPLATE);
}

/// Read the first non-empty `providers.*.api_key` from `~/.kimi-code/config.toml`.
pub fn read_api_key() -> String {
    let Some(home) = dirs::home_dir() else {
        return String::new();
    };
    let path = home.join(".kimi-code").join("config.toml");
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    read_api_key_from(&content)
}

/// Persist `api_key` into `~/.kimi-code/config.toml`, preserving formatting.
pub fn sync_api_key(api_key: &str) {
    let Some(home) = dirs::home_dir() else { return };
    sync_api_key_at(&home, api_key);
}

// ── internals (dir-scoped / pure, for tests) ─────────────────────────

fn credential_valid(segments: &[&str]) -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let mut path = home;
    for s in segments {
        path.push(s);
    }
    let Ok(data) = std::fs::read(&path) else {
        return false;
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&data) else {
        return false;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // The file's top-level `expires_at` mirrors the short-lived access token
    // (kimi re-mints it from the refresh token every `expires_in` seconds
    // while running), so it goes stale within minutes of the CLI being idle
    // even though the login session is still good. Session validity is the
    // refresh token's own `exp` claim, not this field.
    if let Some(refresh) = v.get("refresh_token").and_then(|r| r.as_str())
        && let Some(exp) = jwt_exp(refresh)
    {
        return exp > now;
    }

    let expires_at = v.get("expires_at").and_then(|e| e.as_u64()).unwrap_or(0);
    expires_at > now
}

/// Decode a JWT's `exp` claim without verifying the signature — used only to
/// check whether agentline's cached login session has gone stale.
fn jwt_exp(jwt: &str) -> Option<u64> {
    use base64::Engine;
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("exp").and_then(|e| e.as_u64())
}

// Kept (unused) for the day kimi's ACP server accepts the config api_key —
// then login detection could fall back to it.
#[allow(dead_code)]
fn check_config_provider_key() -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let path = home.join(".kimi-code").join("config.toml");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(doc) = content.parse::<toml::Value>() else {
        return false;
    };
    let Some(providers) = doc.get("providers").and_then(|p| p.as_table()) else {
        return false;
    };
    providers.values().any(|p| {
        let has_api_key = p
            .get("api_key")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty());
        let has_env_key = p.get("env").and_then(|e| e.as_table()).is_some_and(|env| {
            env.values()
                .any(|v| v.as_str().is_some_and(|s| !s.is_empty()))
        });
        has_api_key || has_env_key
    })
}

fn read_api_key_from(content: &str) -> String {
    let Ok(doc) = content.parse::<toml::Value>() else {
        return String::new();
    };
    let Some(providers) = doc.get("providers").and_then(|p| p.as_table()) else {
        return String::new();
    };
    for p in providers.values() {
        if let Some(key) = p.get("api_key").and_then(|v| v.as_str())
            && !key.is_empty()
        {
            return key.to_string();
        }
    }
    String::new()
}

fn sync_api_key_at(home: &Path, api_key: &str) {
    let dir = home.join(".kimi-code");
    let path = dir.join("config.toml");
    let _ = std::fs::create_dir_all(&dir);

    // Edit existing config in place if it has a [providers.*] block; otherwise
    // start from the complete template. Never write a bare api_key.
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let base = if existing.contains("[providers.") {
        existing
    } else {
        CONFIG_TEMPLATE.to_string()
    };
    if let Some(out) = set_api_key_in(&base, api_key) {
        let _ = std::fs::write(&path, out);
    }
}

/// Pure: set `api_key` on every `[providers.*]` table, preserving comments/order
/// (toml_edit, not toml::Value which would flatten to a JSON-like inline blob).
fn set_api_key_in(content: &str, api_key: &str) -> Option<String> {
    let mut doc = content.parse::<toml_edit::DocumentMut>().ok()?;
    let providers = doc.get_mut("providers").and_then(|p| p.as_table_mut())?;
    let mut any = false;
    for (_, p) in providers.iter_mut() {
        if let Some(tbl) = p.as_table_mut() {
            tbl["api_key"] = toml_edit::value(api_key);
            any = true;
        }
    }
    any.then(|| doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_jwt(exp: u64) -> String {
        use base64::Engine;
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("{}");
        let payload =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!("{{\"exp\":{exp}}}"));
        format!("{header}.{payload}.sig")
    }

    #[test]
    fn jwt_exp_decodes_claim() {
        assert_eq!(jwt_exp(&make_jwt(1_700_000_000)), Some(1_700_000_000));
    }

    #[test]
    fn jwt_exp_none_for_garbage() {
        assert_eq!(jwt_exp("not-a-jwt"), None);
    }

    #[test]
    fn set_key_preserves_comments_and_order() {
        let src = "\
# top comment
default_model = \"kimi-code/kimi-for-coding\"

[providers.\"managed:kimi-code\"]
type = \"kimi\"      # inline comment
api_key = \"\"

[[hooks]]
event = \"PreToolUse\"
";
        let out = set_api_key_in(src, "sk-LIVE").unwrap();
        assert!(out.contains("# top comment"));
        assert!(out.contains("# inline comment"));
        assert!(out.contains("[[hooks]]"));
        assert!(out.contains("api_key = \"sk-LIVE\""));
        assert!(!out.trim_start().starts_with('{'), "flattened: {out}");
    }

    #[test]
    fn set_key_none_without_providers() {
        assert!(set_api_key_in("default_model = \"x\"\n", "sk").is_none());
    }

    #[test]
    fn read_key_returns_first_nonempty() {
        assert_eq!(read_api_key_from(CONFIG_TEMPLATE), "");
        let with = set_api_key_in(CONFIG_TEMPLATE, "sk-READ").unwrap();
        assert_eq!(read_api_key_from(&with), "sk-READ");
    }

    #[test]
    fn sync_fresh_writes_complete_template_not_bare_key() {
        let home = tempfile::tempdir().unwrap();
        sync_api_key_at(home.path(), "sk-FRESH");
        let written =
            std::fs::read_to_string(home.path().join(".kimi-code").join("config.toml")).unwrap();
        assert!(written.contains("default_model ="));
        assert!(written.contains("[providers.\"managed:kimi-code\"]"));
        assert!(written.contains("[models.\"kimi-code/kimi-for-coding\"]"));
        assert!(written.contains("api_key = \"sk-FRESH\""));
        assert_eq!(read_api_key_from(&written), "sk-FRESH");
    }

    #[test]
    fn sync_preserves_existing_user_config() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".kimi-code");
        std::fs::create_dir_all(&dir).unwrap();
        let user = "\
default_permission_mode = \"manual\"  # mine

[providers.\"managed:kimi-code\"]
type = \"kimi\"
api_key = \"\"

[[permission.rules]]
decision = \"deny\"
pattern = \"Bash(rm -rf*)\"
";
        std::fs::write(dir.join("config.toml"), user).unwrap();
        sync_api_key_at(home.path(), "sk-USER");
        let written = std::fs::read_to_string(dir.join("config.toml")).unwrap();
        assert!(written.contains("# mine"));
        assert!(written.contains("[[permission.rules]]"));
        assert!(written.contains("pattern = \"Bash(rm -rf*)\""));
        assert!(written.contains("api_key = \"sk-USER\""));
    }
}
