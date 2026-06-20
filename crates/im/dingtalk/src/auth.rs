//! Open the DingTalk Stream connection: HTTP handshake that returns the
//! WebSocket endpoint URL and ticket.
//!
//! Also provides [`TokenManager`] for OpenAPI access_token management
//! (required by interactive card / streaming card APIs).

use crate::error::{Error, Result};
use crate::types::{AccessTokenResp, ConnectionOpenReq, ConnectionOpenResp, Subscription};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

pub const DEFAULT_OPEN_API_HOST: &str = "https://api.dingtalk.com";
pub const CONNECTION_OPEN_PATH: &str = "/v1.0/gateway/connections/open";
const ACCESS_TOKEN_PATH: &str = "/v1.0/oauth2/accessToken";
const REFRESH_INTERVAL: Duration = Duration::from_secs(6900); // 7200 - 300

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

// ── TokenManager ──────────────────────────────────────────────────

#[derive(Clone)]
pub struct TokenManager {
    inner: Arc<Inner>,
}

struct Inner {
    app_key: String,
    app_secret: String,
    http: reqwest::Client,
    token: RwLock<String>,
}

impl TokenManager {
    pub async fn new(app_key: String, app_secret: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| Error::http(format!("build http: {e}")))?;
        let mgr = Self {
            inner: Arc::new(Inner {
                app_key,
                app_secret,
                http,
                token: RwLock::new(String::new()),
            }),
        };
        mgr.refresh().await?;
        Ok(mgr)
    }

    pub async fn token(&self) -> String {
        self.inner.token.read().await.clone()
    }

    pub async fn refresh(&self) -> Result<()> {
        let url = format!("{DEFAULT_OPEN_API_HOST}{ACCESS_TOKEN_PATH}");
        let body = serde_json::json!({
            "appKey": self.inner.app_key,
            "appSecret": self.inner.app_secret,
        });
        let resp = self
            .inner
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::http(format!("POST access_token: {e}")))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::http(format!("read body: {e}")))?;
        if !status.is_success() {
            return Err(Error::Api(format!(
                "access_token → {}: {}",
                status,
                String::from_utf8_lossy(&bytes)
            )));
        }
        let parsed: AccessTokenResp = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Parse(format!("access_token resp: {e}")))?;
        if parsed.access_token.is_empty() {
            return Err(Error::Api("access_token is empty".into()));
        }
        *self.inner.token.write().await = parsed.access_token;
        tracing::debug!(
            "dingtalk access_token refreshed (expires in {}s)",
            parsed.expire_in
        );
        Ok(())
    }

    pub fn spawn_refresh(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(REFRESH_INTERVAL).await;
                if let Err(e) = self.refresh().await {
                    tracing::error!(error=%e, "dingtalk access_token refresh failed");
                }
            }
        })
    }
}
