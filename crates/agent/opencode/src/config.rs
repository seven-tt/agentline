//! opencode's native config / credential management (self-contained).
//!
//! - provider config: `~/.config/opencode/opencode.json`
//! - login credentials: `~/.local/share/opencode/auth.json` (`opencode auth login`)

use std::path::PathBuf;

fn config_path() -> Option<PathBuf> {
    Some(
        dirs::home_dir()?
            .join(".config")
            .join("opencode")
            .join("opencode.json"),
    )
}

fn auth_path() -> Option<PathBuf> {
    Some(
        dirs::home_dir()?
            .join(".local")
            .join("share")
            .join("opencode")
            .join("auth.json"),
    )
}

/// Logged in if a provider is configured (apiKey/baseURL) or `opencode auth
/// login` wrote credentials.
pub fn is_logged_in() -> bool {
    let cfg = config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    if provider_configured(&cfg) {
        return true;
    }
    let auth = auth_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    auth_is_valid(&auth)
}

/// Seed a default provider config on first install when none exists.
pub fn post_install() {
    let Some(path) = config_path() else { return };
    if path.exists()
        && let Ok(content) = std::fs::read_to_string(&path)
        && content.contains("\"provider\"")
    {
        return;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(
        &path,
        r#"{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "custom": {
      "api": "openai",
      "name": "Custom Provider",
      "options": {
        "apiKey": "",
        "baseURL": ""
      },
      "models": {
        "custom-model": {
          "id": "",
          "name": "Custom Model",
          "cost": { "input": 0, "output": 0 },
          "limit": { "context": 128000, "output": 4096 }
        }
      }
    }
  }
}
"#,
    );
}

/// Read the first configured provider's `(apiKey, baseURL)`.
pub fn read_config() -> (String, String) {
    let Some(path) = config_path() else {
        return (String::new(), String::new());
    };
    let Ok(data) = std::fs::read(&path) else {
        return (String::new(), String::new());
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&data) else {
        return (String::new(), String::new());
    };
    let Some(providers) = v.get("provider").and_then(|p| p.as_object()) else {
        return (String::new(), String::new());
    };
    for p in providers.values() {
        if let Some(opts) = p.get("options").and_then(|o| o.as_object()) {
            let api_key = opts
                .get("apiKey")
                .and_then(|k| k.as_str())
                .unwrap_or("")
                .to_string();
            let base_url = opts
                .get("baseURL")
                .and_then(|k| k.as_str())
                .unwrap_or("")
                .to_string();
            if !api_key.is_empty() || !base_url.is_empty() {
                return (api_key, base_url);
            }
        }
    }
    (String::new(), String::new())
}

/// Persist `apiKey` / `baseURL` into every provider's options.
pub fn sync_config(api_key: Option<&str>, base_url: Option<&str>) {
    let Some(path) = config_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let needs_init = !path.exists()
        || std::fs::read_to_string(&path)
            .map(|c| !c.contains("\"provider\""))
            .unwrap_or(true);
    if needs_init {
        post_install();
    }
    if let Ok(data) = std::fs::read(&path)
        && let Ok(mut v) = serde_json::from_slice::<serde_json::Value>(&data)
        && let Some(providers) = v.get_mut("provider").and_then(|p| p.as_object_mut())
    {
        for p in providers.values_mut() {
            if let Some(opts) = p.get_mut("options").and_then(|o| o.as_object_mut()) {
                if let Some(key) = api_key {
                    opts.insert("apiKey".into(), serde_json::Value::String(key.into()));
                }
                if let Some(url) = base_url {
                    opts.insert("baseURL".into(), serde_json::Value::String(url.into()));
                }
            }
        }
        if let Ok(json) = serde_json::to_string_pretty(&v) {
            let _ = std::fs::write(&path, json);
        }
    }
}

/// Pure: a provider counts as configured when it has a non-empty `apiKey` *or*
/// `baseURL`. Local/self-hosted providers (ollama, lm-studio) authenticate via
/// the endpoint and need no key.
fn provider_configured(content: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(content) else {
        return false;
    };
    let Some(providers) = v.get("provider").and_then(|p| p.as_object()) else {
        return false;
    };
    providers.values().any(|p| {
        let opts = p.get("options");
        let nonempty = |key: &str| {
            opts.and_then(|o| o.get(key))
                .and_then(|k| k.as_str())
                .is_some_and(|s| !s.is_empty())
        };
        nonempty("apiKey") || nonempty("baseURL")
    })
}

/// Pure: a non-empty JSON object means at least one provider credential.
fn auth_is_valid(content: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|v| v.as_object().map(|o| !o.is_empty()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_validity() {
        assert!(!auth_is_valid(""));
        assert!(!auth_is_valid("{}"));
        assert!(!auth_is_valid("not json"));
        assert!(!auth_is_valid("[]"));
        assert!(auth_is_valid("{\"anthropic\":{\"type\":\"oauth\"}}"));
    }

    #[test]
    fn provider_configured_local_and_cloud() {
        let empty = r#"{"provider":{"custom":{"options":{"apiKey":"","baseURL":""}}}}"#;
        assert!(!provider_configured(empty));
        // local: baseURL only, no apiKey -> configured
        let local = r#"{"provider":{"local":{"options":{"apiKey":"","baseURL":"http://localhost:11434/v1"}}}}"#;
        assert!(provider_configured(local));
        // cloud: apiKey only -> configured
        let cloud = r#"{"provider":{"openai":{"options":{"apiKey":"sk-x"}}}}"#;
        assert!(provider_configured(cloud));
        assert!(!provider_configured("{}"));
        assert!(!provider_configured("nope"));
    }
}
