//! qoder native login detection (self-contained in the qoder crate).
//!
//! qoder authenticates via `qodercli login` (browser/device flow) or the
//! `QODER_PERSONAL_ACCESS_TOKEN` env var (its canonical headless mechanism).

/// `extra_env` is the agent's injected env (from agentline's own config).
pub fn is_logged_in(extra_env: &[(String, String)]) -> bool {
    cli_status_logged_in()
        || extra_env
            .iter()
            .any(|(k, v)| k == "QODER_PERSONAL_ACCESS_TOKEN" && !v.is_empty())
}

fn cli_status_logged_in() -> bool {
    std::process::Command::new("qodercli")
        .arg("status")
        .output()
        .ok()
        .is_some_and(|o| {
            let out = String::from_utf8_lossy(&o.stdout);
            !out.contains("Not logged in")
                && (out.contains("Username:") || out.contains("Account:") || out.contains("Email:"))
        })
}
