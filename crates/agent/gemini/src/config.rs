//! gemini's native credential management (self-contained in the gemini crate).
//!
//! gemini-cli loads the first `.env` it finds searching up from the cwd, then
//! falls back to `~/.gemini/.env` and `~/.env`. We persist the key in
//! `~/.gemini/.env` so gemini authenticates on its own.

use std::path::Path;

fn env_path(home: &Path) -> std::path::PathBuf {
    home.join(".gemini").join(".env")
}

/// Read `GEMINI_API_KEY` (or `GOOGLE_API_KEY`) from `~/.gemini/.env`.
pub fn read_api_key() -> String {
    let Some(home) = dirs::home_dir() else {
        return String::new();
    };
    let content = std::fs::read_to_string(env_path(&home)).unwrap_or_default();
    let key = read_dotenv_var(&content, "GEMINI_API_KEY");
    if !key.is_empty() {
        return key;
    }
    read_dotenv_var(&content, "GOOGLE_API_KEY")
}

/// Persist `GEMINI_API_KEY` into `~/.gemini/.env`.
pub fn sync_api_key(api_key: &str) {
    if let Some(home) = dirs::home_dir() {
        sync_api_key_at(&home, api_key);
    }
}

/// Logged in via: `~/.gemini/.env` key, Google OAuth (`~/.gemini/oauth_creds.json`),
/// the GEMINI/GOOGLE env vars, or an injected key.
pub fn is_logged_in(extra_env: &[(String, String)]) -> bool {
    has_env_credential()
        || dirs::home_dir()
            .map(|h| h.join(".gemini").join("oauth_creds.json").exists())
            .unwrap_or(false)
        || std::env::var("GEMINI_API_KEY").is_ok_and(|v| !v.is_empty())
        || std::env::var("GOOGLE_API_KEY").is_ok_and(|v| !v.is_empty())
        || extra_env
            .iter()
            .any(|(k, v)| (k == "GEMINI_API_KEY" || k == "GOOGLE_API_KEY") && !v.is_empty())
}

/// True when `~/.gemini/.env` carries a non-empty Gemini/Google API key.
pub fn has_env_credential() -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let content = std::fs::read_to_string(env_path(&home)).unwrap_or_default();
    !read_dotenv_var(&content, "GEMINI_API_KEY").is_empty()
        || !read_dotenv_var(&content, "GOOGLE_API_KEY").is_empty()
}

fn sync_api_key_at(home: &Path, api_key: &str) {
    let path = env_path(home);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::write(&path, set_dotenv_var(&existing, "GEMINI_API_KEY", api_key));
}

/// The key part of a dotenv line (`KEY=…` or `export KEY=…`), skipping blanks
/// and comments.
fn dotenv_line_key(line: &str) -> Option<&str> {
    let t = line.trim_start();
    if t.is_empty() || t.starts_with('#') {
        return None;
    }
    let t = t.strip_prefix("export ").unwrap_or(t);
    let key = t.split('=').next()?.trim();
    (!key.is_empty()).then_some(key)
}

/// Pure: set/replace `key=value` in dotenv text (replaces existing line for
/// `key`, with or without `export`; else appends).
fn set_dotenv_var(existing: &str, key: &str, value: &str) -> String {
    let mut replaced = false;
    let mut out: Vec<String> = Vec::new();
    for line in existing.lines() {
        if !replaced && dotenv_line_key(line) == Some(key) {
            out.push(format!("{key}={value}"));
            replaced = true;
        } else {
            out.push(line.to_string());
        }
    }
    if !replaced {
        out.push(format!("{key}={value}"));
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// Pure: read the value of `key` from dotenv text (strips surrounding quotes).
fn read_dotenv_var(content: &str, key: &str) -> String {
    for line in content.lines() {
        if dotenv_line_key(line) == Some(key) {
            let t = line.trim_start();
            let t = t.strip_prefix("export ").unwrap_or(t);
            if let Some((_, v)) = t.split_once('=') {
                return v.trim().trim_matches('"').trim_matches('\'').to_string();
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotenv_append_and_replace() {
        let s = set_dotenv_var("", "GEMINI_API_KEY", "k1");
        assert_eq!(read_dotenv_var(&s, "GEMINI_API_KEY"), "k1");
        let existing = "FOO=bar\nexport GEMINI_API_KEY=old\n";
        let s2 = set_dotenv_var(existing, "GEMINI_API_KEY", "new");
        assert_eq!(read_dotenv_var(&s2, "GEMINI_API_KEY"), "new");
        assert_eq!(read_dotenv_var(&s2, "FOO"), "bar");
        assert_eq!(s2.matches("GEMINI_API_KEY").count(), 1, "duplicated: {s2}");
    }

    #[test]
    fn read_strips_quotes_and_ignores_comments() {
        assert_eq!(
            read_dotenv_var(
                "# GEMINI_API_KEY=ignored\nGEMINI_API_KEY=\"q\"\n",
                "GEMINI_API_KEY"
            ),
            "q"
        );
    }

    #[test]
    fn sync_writes_env_file() {
        let home = tempfile::tempdir().unwrap();
        sync_api_key_at(home.path(), "sk-GEM");
        let content = std::fs::read_to_string(home.path().join(".gemini").join(".env")).unwrap();
        assert_eq!(read_dotenv_var(&content, "GEMINI_API_KEY"), "sk-GEM");
    }
}
