use crate::error::{Error, Result};
use crate::stream::WebhookCache;
use crate::types::SessionWebhookText;
use agentline_bridge::types::PeerRef;
use std::time::Duration;

/// Reply to a peer using the most recent `sessionWebhook` we cached for them.
///
/// DingTalk webhooks expire after a few minutes of inactivity; if we run out
/// of fresh webhook this returns `Error::Api(...)` and the caller can decide
/// whether to fall back to the work-notification API.
pub async fn send_text(
    http: &reqwest::Client,
    webhooks: &WebhookCache,
    to: &PeerRef,
    text: &str,
) -> Result<()> {
    let webhook = webhooks
        .lock()
        .await
        .get(&to.user_id)
        .cloned()
        .ok_or_else(|| Error::Api(format!("no session webhook cached for {}", to.user_id)))?;

    let body = SessionWebhookText::new(text);
    let resp = http
        .post(&webhook)
        .timeout(Duration::from_secs(15))
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::http(format!("POST session_webhook: {e}")))?;
    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| Error::http(format!("read body: {e}")))?;
    if !status.is_success() {
        return Err(Error::Api(format!(
            "session webhook → {}: {}",
            status,
            String::from_utf8_lossy(&bytes)
        )));
    }
    Ok(())
}
