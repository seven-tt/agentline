//! hermes native login detection (self-contained in the hermes crate).
//!
//! hermes stores its OAuth login at `~/.hermes/auth.json` (via
//! `hermes setup --portal`).

pub fn is_logged_in() -> bool {
    dirs::home_dir()
        .map(|h| h.join(".hermes").join("auth.json").exists())
        .unwrap_or(false)
}
