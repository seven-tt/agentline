use agentline_im_wechat::CursorPersist;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppState {
    #[serde(default)]
    pub im: ImState,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImState {
    #[serde(default)]
    pub wechat: WechatState,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WechatState {
    #[serde(default)]
    pub bot_token: Option<String>,
    #[serde(default)]
    pub bot_baseurl: Option<String>,
    #[serde(default)]
    pub get_updates_buf: String,
    /// Latest known context_token per user_id. Updated on every inbound
    /// message so that the channel can reach a known user after restart
    /// even before a new message arrives.
    #[serde(default)]
    pub context_tokens: HashMap<String, String>,
}

impl AppState {
    pub fn load_or_default(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read state {}", path.display()))?;
        let state: Self = serde_json::from_str(&text)
            .with_context(|| format!("parse state {}", path.display()))?;
        Ok(state)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir {}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, text).with_context(|| format!("write state {}", path.display()))?;
        Ok(())
    }
}

/// Persist iLink long-poll cursor changes back to state.json on every advance.
/// Cheap (a few hundred bytes); good enough until we add proper async I/O.
pub struct FileCursorPersist {
    path: PathBuf,
}

impl FileCursorPersist {
    pub fn new(path: PathBuf) -> Arc<Self> {
        Arc::new(Self { path })
    }
}

impl CursorPersist for FileCursorPersist {
    fn save(&self, cursor: &str) {
        let mut state = AppState::load_or_default(&self.path).unwrap_or_default();
        state.im.wechat.get_updates_buf = cursor.to_string();
        if let Err(e) = state.save(&self.path) {
            tracing::error!(error=%e, "failed to persist cursor");
        }
    }

    fn save_context_token(&self, user_id: &str, token: &str) {
        let mut state = AppState::load_or_default(&self.path).unwrap_or_default();
        state
            .im
            .wechat
            .context_tokens
            .insert(user_id.to_string(), token.to_string());
        if let Err(e) = state.save(&self.path) {
            tracing::error!(error=%e, "failed to persist context_token for {user_id}");
        }
    }
}
