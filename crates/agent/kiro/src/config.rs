//! kiro native login detection (self-contained in the kiro crate).
//!
//! kiro authenticates via `kiro-cli` (AWS login) or the `KIRO_API_KEY` env var.

pub fn is_logged_in() -> bool {
    std::env::var("KIRO_API_KEY").is_ok_and(|v| !v.is_empty()) || cli_whoami()
}

fn cli_whoami() -> bool {
    std::process::Command::new("kiro-cli")
        .arg("whoami")
        .output()
        .ok()
        .is_some_and(|o| o.status.success())
}
