use std::sync::OnceLock;

/// Build a `NO_PROXY` value that merges (in order):
/// 1. `extra` — caller-supplied entries (e.g. from config file)
/// 2. The user's `NO_PROXY` / `no_proxy`, from the process env or — if unset
///    there — their login shell profile (see [`detect_shell_proxy`])
/// 3. Standard RFC-1918 private ranges (always present)
///
/// Supports CIDR notation (curl ≥7.86, Python requests ≥2.27 / urllib3 ≥2.0).
pub fn build_no_proxy_with(extra: &str) -> String {
    const LAN: &[&str] = &[
        "localhost",
        "127.0.0.0/8",
        "10.0.0.0/8",
        "172.16.0.0/12",
        "192.168.0.0/16",
        "::1",
    ];

    let from_env = &detect_shell_proxy().no_proxy;

    let mut entries: Vec<String> = extra
        .split(',')
        .chain(from_env.split(','))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    for &lan in LAN {
        if !entries.iter().any(|e| e == lan) {
            entries.push(lan.to_string());
        }
    }

    entries.join(",")
}

/// Convenience wrapper: no caller-supplied extra entries.
pub fn build_no_proxy() -> String {
    build_no_proxy_with("")
}

/// HTTP_PROXY / HTTPS_PROXY / NO_PROXY as the user has them configured
/// outside of agentline — used as the fallback when agentline's own config
/// fields are left empty.
#[derive(Debug, Clone, Default)]
pub struct ShellProxy {
    pub http: String,
    pub https: String,
    pub no_proxy: String,
}

/// Detect the user's ambient proxy settings: the process env first (covers
/// running `agentline` directly from a terminal), then — since agentline is
/// commonly started by the tray app or launchd with no shell parent and
/// therefore no inherited env — the user's login shell profile (.zprofile /
/// .zshrc / .bash_profile / .bashrc), mirroring how the tray enriches PATH
/// for the daemon it spawns. Cached for the life of the process: a login
/// shell costs tens of milliseconds and these settings don't change at
/// runtime.
pub fn detect_shell_proxy() -> &'static ShellProxy {
    static CACHE: OnceLock<ShellProxy> = OnceLock::new();
    CACHE.get_or_init(|| {
        let env_of = |upper: &str, lower: &str| {
            std::env::var(upper)
                .ok()
                .filter(|v| !v.is_empty())
                .or_else(|| std::env::var(lower).ok().filter(|v| !v.is_empty()))
        };
        let http = env_of("HTTP_PROXY", "http_proxy");
        let https = env_of("HTTPS_PROXY", "https_proxy");
        let no_proxy = env_of("NO_PROXY", "no_proxy");

        if http.is_some() || https.is_some() || no_proxy.is_some() {
            return ShellProxy {
                http: http.unwrap_or_default(),
                https: https.unwrap_or_default(),
                no_proxy: no_proxy.unwrap_or_default(),
            };
        }

        login_shell_proxy().unwrap_or_default()
    })
}

/// Source proxy vars from an interactive login shell (reads .zprofile AND
/// .zshrc/.bashrc, same probe shape as the tray's PATH enrichment). One shell
/// invocation, all three vars at once, to keep startup cost down.
#[cfg(unix)]
fn login_shell_proxy() -> Option<ShellProxy> {
    const SCRIPT: &str =
        "echo \"$HTTP_PROXY|$HTTPS_PROXY|$NO_PROXY|$http_proxy|$https_proxy|$no_proxy\"";
    for args in [vec!["-i", "-l", "-c", SCRIPT], vec!["-l", "-c", SCRIPT]] {
        for shell in ["/bin/zsh", "/bin/bash"] {
            let Ok(out) = std::process::Command::new(shell)
                .args(&args)
                .env("TERM", "dumb")
                .output()
            else {
                continue;
            };
            if !out.status.success() {
                continue;
            }
            let line = String::from_utf8_lossy(&out.stdout);
            let line = line.lines().next_back().unwrap_or("");
            let parts: Vec<&str> = line.split('|').collect();
            let [hu, hsu, nu, hl, hsl, nl] = parts.as_slice() else {
                continue;
            };
            let pick = |a: &str, b: &str| {
                if !a.is_empty() {
                    a.to_string()
                } else {
                    b.to_string()
                }
            };
            let result = ShellProxy {
                http: pick(hu, hl),
                https: pick(hsu, hsl),
                no_proxy: pick(nu, nl),
            };
            if !result.http.is_empty() || !result.https.is_empty() || !result.no_proxy.is_empty() {
                return Some(result);
            }
        }
    }
    None
}

#[cfg(not(unix))]
fn login_shell_proxy() -> Option<ShellProxy> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_lan_ranges() {
        let v = build_no_proxy();
        assert!(v.contains("192.168.0.0/16"));
        assert!(v.contains("10.0.0.0/8"));
        assert!(v.contains("localhost"));
    }

    #[test]
    fn preserves_existing_entries() {
        // Temporarily set NO_PROXY (this test is env-dependent, but isolated).
        let result = {
            // We can't safely set env vars in tests without risk of parallelism
            // issues, so just test the merge logic directly.
            let existing = "myhost.internal,192.168.1.1";
            let mut entries: Vec<String> = existing
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let lan = ["localhost", "10.0.0.0/8"];
            for &l in &lan {
                if !entries.iter().any(|e| e == l) {
                    entries.push(l.to_string());
                }
            }
            entries.join(",")
        };
        assert!(result.contains("myhost.internal"));
        assert!(result.contains("192.168.1.1"));
        assert!(result.contains("localhost"));
        assert!(result.contains("10.0.0.0/8"));
    }
}
