//! codex's native credential management (self-contained in the codex crate).
//!
//! codex reads its API key from `~/.codex/auth.json` (`{"OPENAI_API_KEY": …}`),
//! the same file `codex login --with-api-key` writes — so storing it there means
//! codex authenticates whether launched by agentline or run standalone.

use std::path::Path;

fn auth_path(home: &Path) -> std::path::PathBuf {
    home.join(".codex").join("auth.json")
}

/// Logged in via: an injected `OPENAI_API_KEY`, the env var, or
/// `~/.codex/auth.json` (written by `codex login` / ChatGPT sign-in).
pub fn is_logged_in(extra_env: &[(String, String)]) -> bool {
    extra_env
        .iter()
        .any(|(k, v)| k == "OPENAI_API_KEY" && !v.is_empty())
        || std::env::var("OPENAI_API_KEY").is_ok_and(|v| !v.is_empty())
        || dirs::home_dir()
            .map(|h| auth_path(&h).exists())
            .unwrap_or(false)
}

/// Read `OPENAI_API_KEY` from `~/.codex/auth.json`.
pub fn read_api_key() -> String {
    let Some(home) = dirs::home_dir() else {
        return String::new();
    };
    let content = std::fs::read_to_string(auth_path(&home)).unwrap_or_default();
    read_api_key_from(&content)
}

/// Persist `OPENAI_API_KEY` into `~/.codex/auth.json` (preserving other keys).
pub fn sync_api_key(api_key: &str) {
    if let Some(home) = dirs::home_dir() {
        sync_api_key_at(&home, api_key);
    }
}

fn sync_api_key_at(home: &Path, api_key: &str) {
    let path = auth_path(home);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::write(&path, set_api_key_in(&existing, api_key));
}

/// Pure: merge `OPENAI_API_KEY` into existing auth.json text (or a fresh
/// object), preserving any other keys. Returns pretty-printed JSON.
fn set_api_key_in(existing: &str, api_key: &str) -> String {
    let mut v = serde_json::from_str::<serde_json::Value>(existing)
        .ok()
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    v["OPENAI_API_KEY"] = serde_json::Value::String(api_key.to_string());
    serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
}

/// Pure: read `OPENAI_API_KEY` out of auth.json text.
fn read_api_key_from(content: &str) -> String {
    serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|v| {
            v.get("OPENAI_API_KEY")
                .and_then(|k| k.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_key_fresh_and_merge() {
        let fresh = set_api_key_in("", "sk-A");
        assert_eq!(read_api_key_from(&fresh), "sk-A");
        let merged = set_api_key_in("{\"tokens\":{\"id\":\"x\"}}", "sk-B");
        assert_eq!(read_api_key_from(&merged), "sk-B");
        assert!(merged.contains("tokens"), "dropped existing keys: {merged}");
    }

    #[test]
    fn sync_writes_auth_json() {
        let home = tempfile::tempdir().unwrap();
        sync_api_key_at(home.path(), "sk-CODEX");
        let content =
            std::fs::read_to_string(home.path().join(".codex").join("auth.json")).unwrap();
        assert_eq!(read_api_key_from(&content), "sk-CODEX");
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(v["OPENAI_API_KEY"], "sk-CODEX");
    }
}
