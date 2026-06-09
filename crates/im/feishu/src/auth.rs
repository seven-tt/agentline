use crate::error::{Error, Result};
use crate::types::{TenantTokenReq, TenantTokenResp};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

const TOKEN_URL: &str =
    "https://open.feishu.cn/open-apis/auth/v3/tenant_access_token/internal/";

/// Refresh interval: 1.5 hours (token TTL is 2h).
const REFRESH_INTERVAL: Duration = Duration::from_secs(90 * 60);

#[derive(Clone)]
pub struct TokenManager {
    inner: Arc<Inner>,
}

struct Inner {
    app_id: String,
    app_secret: String,
    http: reqwest::Client,
    token: RwLock<String>,
}

impl TokenManager {
    pub async fn new(app_id: String, app_secret: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| Error::http(format!("build http: {e}")))?;
        let mgr = Self {
            inner: Arc::new(Inner {
                app_id,
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
        let body = TenantTokenReq {
            app_id: self.inner.app_id.clone(),
            app_secret: self.inner.app_secret.clone(),
        };
        let resp = self
            .inner
            .http
            .post(TOKEN_URL)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::http(format!("POST token: {e}")))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::http(format!("read body: {e}")))?;
        if !status.is_success() {
            return Err(Error::Auth(format!(
                "token api → {}: {}",
                status,
                String::from_utf8_lossy(&bytes)
            )));
        }
        let parsed: TenantTokenResp =
            serde_json::from_slice(&bytes).map_err(|e| Error::Parse(format!("token resp: {e}")))?;
        if parsed.code != 0 {
            return Err(Error::Auth(format!(
                "token api code={}: {}",
                parsed.code, parsed.msg
            )));
        }
        *self.inner.token.write().await = parsed.tenant_access_token;
        tracing::debug!("feishu token refreshed (expires in {}s)", parsed.expire);
        Ok(())
    }

    /// Spawn background refresh task. Returns a JoinHandle that runs until
    /// the TokenManager is dropped or the task is aborted.
    pub fn spawn_refresh(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(REFRESH_INTERVAL).await;
                if let Err(e) = self.refresh().await {
                    tracing::error!(error=%e, "feishu token refresh failed");
                }
            }
        })
    }
}
