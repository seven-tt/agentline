use crate::error::{Error, Result};
use crate::http::HttpClient;
use crate::types::{GetQrcodeResp, QrcodeStatusResp};
use std::time::Duration;

const BOT_TYPE: &str = "3";
const POLL_INTERVAL_SECS: u64 = 1;
const MAX_POLL_SECS: u64 = 480;
const QR_LONG_POLL_TIMEOUT_SECS: u64 = 35;

/// Output of [`request_qr`].
#[derive(Debug, Clone)]
pub struct QrCode {
    /// The WeChat login URL — encode this as a QR code for the user to scan.
    pub login_url: String,
    /// Opaque token; pass back into [`wait_for_scan`].
    pub token: String,
}

#[derive(Debug, Clone)]
pub struct LoginResult {
    pub bot_token: String,
    pub baseurl: Option<String>,
}

/// Phase 1 — fetch the QR-code login URL from iLink via POST.
pub async fn request_qr(http: &HttpClient) -> Result<QrCode> {
    let path = format!("/ilink/bot/get_bot_qrcode?bot_type={BOT_TYPE}");
    // Include any existing bot tokens so the server can skip re-auth if already bound.
    let body = serde_json::json!({ "local_token_list": [] });
    let resp: GetQrcodeResp = http.post_json(&path, &body).await?;
    Ok(QrCode {
        login_url: resp.qrcode_img_content,
        token: resp.qrcode,
    })
}

/// Phase 2 — poll for scan completion.
pub async fn wait_for_scan(http: &HttpClient, qr: &QrCode) -> Result<LoginResult> {
    let started = std::time::Instant::now();
    // polling base URL may be redirected by the server
    let mut api_base: Option<String> = None;

    loop {
        if started.elapsed().as_secs() > MAX_POLL_SECS {
            return Err(Error::Login(format!("login timed out after {MAX_POLL_SECS}s")));
        }
        let path = format!("/ilink/bot/get_qrcode_status?qrcode={}", qr.token);
        let status: QrcodeStatusResp = match http
            .get_json_with_base(api_base.as_deref(), &path, Some(Duration::from_secs(QR_LONG_POLL_TIMEOUT_SECS)))
            .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error=%e, "qrcode status poll failed; retrying");
                tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
                continue;
            }
        };

        match status.status.as_str() {
            "confirmed" => {
                let token = status
                    .bot_token
                    .ok_or_else(|| Error::Login("confirmed without bot_token".into()))?;
                http.set_token(token.clone()).await;
                if let Some(b) = status.baseurl.clone() {
                    http.set_base_url(b).await;
                }
                return Ok(LoginResult {
                    bot_token: token,
                    baseurl: status.baseurl,
                });
            }
            "expired" => return Err(Error::Login("QR code expired".into())),
            // API typo: "scaned" not "scanned"
            "scaned" => tracing::info!("scanned, awaiting confirm…"),
            "scaned_but_redirect" => {
                if let Some(host) = &status.redirect_host {
                    api_base = Some(format!("https://{host}"));
                    tracing::info!(host=%host, "IDC redirect");
                }
            }
            "binded_redirect" => {
                return Err(Error::Login("already bound to another OpenClaw instance".into()));
            }
            "need_verifycode" => {
                tracing::warn!("need_verifycode not supported in web flow");
            }
            "verify_code_blocked" => {
                return Err(Error::Login("verify code blocked, please retry later".into()));
            }
            "wait" | _ => {
                tracing::debug!(status = %status.status, "qr poll");
            }
        }
        tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
    }
}
