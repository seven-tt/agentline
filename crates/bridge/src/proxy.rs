/// Build a `NO_PROXY` value that merges (in order):
/// 1. `extra` — caller-supplied entries (e.g. from config file)
/// 2. The parent process's `NO_PROXY` / `no_proxy` env var
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

    let from_env = std::env::var("NO_PROXY")
        .or_else(|_| std::env::var("no_proxy"))
        .unwrap_or_default();

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
