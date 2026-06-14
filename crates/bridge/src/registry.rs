use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize)]
pub struct SessionSnapshot {
    pub id: String,
    pub user: String,
    pub active: bool,
    pub cwd: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ImSnapshot {
    pub healthy: bool,
    pub sessions: Vec<SessionSnapshot>,
}

#[derive(Debug, Default)]
pub struct SessionRegistry {
    inner: Mutex<HashMap<String, ImSnapshot>>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&self, source_id: &str, snapshot: ImSnapshot) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(source_id.to_string(), snapshot);
    }

    pub fn replace(&self, data: HashMap<String, ImSnapshot>) {
        *self.inner.lock().unwrap_or_else(|e| e.into_inner()) = data;
    }

    pub fn snapshot(&self) -> HashMap<String, ImSnapshot> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}
