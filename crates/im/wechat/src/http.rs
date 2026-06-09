use base64::Engine;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use crate::error::{Error, Result};

const DEFAULT_BASE_URL: &str = "https://ilinkai.weixin.qq.com";
const ILINK_APP_ID: &str = "bot";
/// buildClientVersion("2.4.4") = (2<<16)|(4<<8)|4 = 132100
const ILINK_APP_CLIENT_VERSION: u32 = (2 << 16) | (4 << 8) | 4;

/// Shared HTTP wrapper for iLink. Injects the three fixed iLink headers
/// (`AuthorizationType`, `X-WECHAT-UIN`, `Authorization`) on every request,
/// regenerating UIN per call.
#[derive(Clone)]
pub struct HttpClient {
    inner: reqwest::Client,
    base_url: Arc<RwLock<String>>,
    bot_token: Arc<RwLock<Option<String>>>,
}

impl HttpClient {
    pub fn new() -> Result<Self> {
        let inner = reqwest::Client::builder()
            .timeout(Duration::from_secs(40))
            // Close idle connections after 30 s. iLink's long-poll responses
            // return after ~35–45 s; without this the pool reuses connections
            // the server has already closed, producing empty response bodies
            // (EOF) on the next send.
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| Error::other(format!("build http client: {e}")))?;
        Ok(Self {
            inner,
            base_url: Arc::new(RwLock::new(DEFAULT_BASE_URL.to_string())),
            bot_token: Arc::new(RwLock::new(None)),
        })
    }

    pub async fn set_base_url(&self, url: String) {
        if !url.is_empty() {
            *self.base_url.write().await = url;
        }
    }

    pub async fn set_token(&self, token: String) {
        *self.bot_token.write().await = Some(token);
    }

    pub async fn token(&self) -> Option<String> {
        self.bot_token.read().await.clone()
    }

    pub async fn base_url(&self) -> String {
        self.base_url.read().await.clone()
    }

    /// Common headers sent on every request (GET and POST).
    fn build_common_headers() -> HeaderMap {
        let mut h = HeaderMap::new();
        let v = HeaderValue::from_static(ILINK_APP_ID);
        h.insert("iLink-App-Id", v);
        if let Ok(v) = HeaderValue::from_str(&ILINK_APP_CLIENT_VERSION.to_string()) {
            h.insert("iLink-App-ClientVersion", v);
        }
        h
    }

    /// Full headers for POST requests (adds auth fields on top of common headers).
    async fn build_post_headers(&self) -> HeaderMap {
        let mut h = Self::build_common_headers();
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        h.insert(
            "AuthorizationType",
            HeaderValue::from_static("ilink_bot_token"),
        );
        let uin =
            base64::engine::general_purpose::STANDARD.encode(rand::random::<u32>().to_string());
        if let Ok(v) = HeaderValue::from_str(&uin) {
            h.insert("X-WECHAT-UIN", v);
        }
        if let Some(t) = self.bot_token.read().await.as_ref() {
            if let Ok(v) = HeaderValue::from_str(&format!("Bearer {t}")) {
                h.insert(AUTHORIZATION, v);
            }
        }
        h
    }

    fn join(&self, base: &str, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else if path.starts_with('/') {
            format!("{}{}", base.trim_end_matches('/'), path)
        } else {
            format!("{}/{}", base.trim_end_matches('/'), path)
        }
    }

    /// GET with common headers only (no auth). Supports custom base URL and timeout.
    /// Used for status polling which uses a different base URL on IDC redirect.
    pub async fn get_json_with_base<T: DeserializeOwned>(
        &self,
        base_override: Option<&str>,
        path: &str,
        timeout: Option<Duration>,
    ) -> Result<T> {
        let base = match base_override {
            Some(b) => b.to_string(),
            None => self.base_url.read().await.clone(),
        };
        let url = self.join(&base, path);
        let mut req = self.inner.get(&url).headers(Self::build_common_headers());
        if let Some(t) = timeout {
            req = req.timeout(t);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Http(format!("GET {url}: {e}")))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Http(format!("read {url}: {e}")))?;
        if !status.is_success() {
            return Err(Error::Http(format!(
                "GET {url} → {status}: {}",
                String::from_utf8_lossy(&bytes)
            )));
        }
        serde_json::from_slice::<T>(&bytes).map_err(|e| Error::Http(format!("decode {url}: {e}")))
    }

    pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.get_json_with_base(None, path, None).await
    }

    /// Raw GET returning bytes. Used for CDN media download.
    pub async fn get_bytes(&self, url: &str) -> Result<bytes::Bytes> {
        let resp = self
            .inner
            .get(url)
            .headers(Self::build_common_headers())
            .send()
            .await
            .map_err(|e| Error::Http(format!("GET {url}: {e}")))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Http(format!("read {url}: {e}")))?;
        if !status.is_success() {
            return Err(Error::Http(format!(
                "GET {url} → {status}: {}",
                String::from_utf8_lossy(&bytes)
            )));
        }
        Ok(bytes)
    }

    pub async fn post_json<Req, Resp>(&self, path: &str, body: &Req) -> Result<Resp>
    where
        Req: Serialize + ?Sized,
        Resp: DeserializeOwned,
    {
        self.post_json_with_timeout(path, body, None).await
    }

    pub async fn post_json_with_timeout<Req, Resp>(
        &self,
        path: &str,
        body: &Req,
        timeout: Option<Duration>,
    ) -> Result<Resp>
    where
        Req: Serialize + ?Sized,
        Resp: DeserializeOwned,
    {
        match self.do_post(path, body, timeout).await {
            // Empty response body → stale pooled connection; retry once with a
            // fresh connection (pool_idle_timeout prevents this normally, but
            // a retry covers the remaining race window).
            Err(Error::Http(ref msg)) if msg.contains("EOF while parsing") => {
                tracing::warn!(path, "empty response body (stale connection?); retrying");
                tokio::time::sleep(Duration::from_millis(300)).await;
                self.do_post(path, body, timeout).await
            }
            other => other,
        }
    }

    async fn do_post<Req, Resp>(
        &self,
        path: &str,
        body: &Req,
        timeout: Option<Duration>,
    ) -> Result<Resp>
    where
        Req: Serialize + ?Sized,
        Resp: DeserializeOwned,
    {
        let base = self.base_url.read().await.clone();
        let url = self.join(&base, path);
        let mut req = self
            .inner
            .post(&url)
            .headers(self.build_post_headers().await)
            .json(body);
        if let Some(t) = timeout {
            req = req.timeout(t);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Http(format!("POST {url}: {e}")))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Http(format!("read {url}: {e}")))?;
        if !status.is_success() {
            return Err(Error::Http(format!(
                "POST {url} → {status}: {}",
                String::from_utf8_lossy(&bytes)
            )));
        }
        serde_json::from_slice::<Resp>(&bytes)
            .map_err(|e| Error::Http(format!("decode {url}: {e}")))
    }
}
