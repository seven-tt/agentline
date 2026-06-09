use crate::error::Result;
use crate::types::{AgentUpdate, SessionId};
use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::Value;
use std::path::Path;

/// A coding agent that can run prompts inside a project working directory.
///
/// Each adapter (ACP, Codex, Kimi, ...) maps its native protocol onto these
/// methods. The `SessionId` newtype is the same shape for everyone (a string)
/// to keep the trait object-safe; adapters encode any internal state behind it.
#[async_trait]
pub trait AgentBackend: Send + Sync + 'static {
    /// Open a fresh session with `cwd` as the working directory.
    async fn new_session(&self, cwd: &Path) -> Result<SessionId>;

    /// Send a user prompt to a session and return a stream of update frames.
    /// The stream ends when the agent finishes the turn (or errors).
    async fn prompt<'a>(
        &'a self,
        sid: &'a SessionId,
        text: &'a str,
    ) -> Result<BoxStream<'a, AgentUpdate>>;

    /// Cancel the in-flight prompt of `sid`. The active stream should
    /// terminate (with a final `AgentUpdate::Done` or `Error`) shortly after.
    async fn cancel(&self, sid: &SessionId) -> Result<()>;

    /// Reply to a permission request previously surfaced via
    /// `AgentUpdate::PermissionRequest`.
    async fn answer_permission(&self, sid: &SessionId, request_id: &str, allow: bool)
    -> Result<()>;

    /// Tear down a session. After this, the SessionId is invalid.
    async fn close_session(&self, sid: SessionId) -> Result<()>;

    /// Reply to an elicitation request previously surfaced via
    /// `AgentUpdate::ElicitInput`.  Default is a no-op for backends that do
    /// not support elicitation.
    async fn answer_elicitation(&self, _elicit_id: &str, _response: Value) -> Result<()> {
        Ok(())
    }

    /// Terminate the backend and any child processes it spawned. Called on
    /// graceful shutdown (e.g. Ctrl-C). Default is a no-op.
    async fn shutdown(&self) {}
}
