//! Open the DingTalk Stream connection: HTTP handshake that returns the
//! WebSocket endpoint URL and ticket.

use crate::error::{Error, Result};
use crate::types::{ConnectionOpenReq, ConnectionOpenResp, Subscription};
use std::time::Duration;

pub const DEFAULT_OPEN_API_HOST: &str = "https://api.dingtalk.com";
pub const CONNECTION_OPEN_PATH: &str = "/v1.0/gateway/connections/open";

/// Topic the bot must subscribe to in order to receive incoming chat messages.
pub const TOPIC_BOT_MESSAGE: &str = "/v1.0/im/bot/messages/get";

#[derive(Debug, Clone)]
pub struct OpenParams {
    pub client_id: String,
    pub client_secret: String,
    pub user_agent: String,
}

pub async fn open_connection(params: &OpenParams) -> Result<ConnectionOpenResp> {
    let body = ConnectionOpenReq {
        client_id: params.client_id.clone(),
        client_secret: params.client_secret.clone(),
        subscriptions: default_subscriptions(),
        ua: params.user_agent.clone(),
        local_ip: best_local_ip().unwrap_or_default(),
        extras: serde_json::Value::Object(Default::default()),
    };
    let url = format!("{DEFAULT_OPEN_API_HOST}{CONNECTION_OPEN_PATH}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| Error::http(format!("build http: {e}")))?;
    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::http(format!("POST {url}: {e}")))?;

    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| Error::http(format!("read body: {e}")))?;
    if !status.is_success() {
        return Err(Error::Api(format!(
            "{} → {}: {}",
            url,
            status,
            String::from_utf8_lossy(&bytes)
        )));
    }
    serde_json::from_slice(&bytes).map_err(|e| Error::Parse(format!("decode handshake: {e}")))
}

fn default_subscriptions() -> Vec<Subscription> {
    vec![
        Subscription {
            kind: "SYSTEM".into(),
            topic: "ping".into(),
        },
        Subscription {
            kind: "SYSTEM".into(),
            topic: "disconnect".into(),
        },
        Subscription {
            kind: "CALLBACK".into(),
            topic: TOPIC_BOT_MESSAGE.into(),
        },
    ]
}

/// Best-effort local IP discovery. The Go SDK does the same; the field is
/// informational and the gateway accepts an empty string.
fn best_local_ip() -> Option<String> {
    // Use a UDP socket trick: connecting to a public IP fills in the local
    // bound address without sending any packet.
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    let local = sock.local_addr().ok()?;
    Some(local.ip().to_string())
}
