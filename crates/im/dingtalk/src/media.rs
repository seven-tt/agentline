//! Media download for DingTalk inbound picture/audio/richText messages.
//!
//! A callback only carries a short-lived `downloadCode`; resolving it to
//! actual bytes is a two-step exchange: POST it (plus the robot's code) to
//! `/v1.0/robot/messageFiles/download` for a temporary `downloadUrl`, then
//! GET that URL for the file content.

use crate::auth::{DEFAULT_OPEN_API_HOST, TokenManager};
use crate::types::{MessageFileDownloadReq, MessageFileDownloadResp};
use std::path::{Path, PathBuf};
use std::time::Duration;

const DOWNLOAD_PATH: &str = "/v1.0/robot/messageFiles/download";

pub fn media_save_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".agentline")
        .join("media")
        .join("inbound")
}

pub async fn download_media(
    http: &reqwest::Client,
    token_mgr: &TokenManager,
    robot_code: &str,
    download_code: &str,
    prefix: &str,
    save_dir: &Path,
) -> Option<PathBuf> {
    let token = token_mgr.token().await;
    let req = MessageFileDownloadReq {
        download_code: download_code.to_string(),
        robot_code: robot_code.to_string(),
    };
    let url = format!("{DEFAULT_OPEN_API_HOST}{DOWNLOAD_PATH}");
    let resp = http
        .post(&url)
        .timeout(Duration::from_secs(15))
        .header("x-acs-dingtalk-access-token", &token)
        .json(&req)
        .send()
        .await
        .map_err(|e| tracing::error!("dingtalk messageFiles/download failed: {e}"))
        .ok()?;

    if !resp.status().is_success() {
        tracing::error!(
            status = %resp.status(),
            "dingtalk messageFiles/download failed for downloadCode={download_code}"
        );
        return None;
    }

    let resolved: MessageFileDownloadResp = resp
        .json()
        .await
        .map_err(|e| tracing::error!("dingtalk messageFiles/download parse failed: {e}"))
        .ok()?;

    let file_resp = http
        .get(&resolved.download_url)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .map_err(|e| tracing::error!("dingtalk media fetch failed: {e}"))
        .ok()?;

    if !file_resp.status().is_success() {
        tracing::error!(status = %file_resp.status(), "dingtalk media fetch failed");
        return None;
    }

    let data = file_resp
        .bytes()
        .await
        .map_err(|e| tracing::error!("dingtalk media read body failed: {e}"))
        .ok()?;

    let ext = infer_ext(&data);
    let filename = format!("{prefix}_{}.{ext}", ulid::Ulid::new());
    let path = save_dir.join(&filename);

    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Err(e) = tokio::fs::write(&path, &data).await {
        tracing::error!("dingtalk media write failed: {e}");
        return None;
    }
    tracing::debug!("dingtalk media saved: {}", path.display());
    Some(path)
}

fn infer_ext(data: &[u8]) -> &'static str {
    if data.starts_with(b"\x89PNG") {
        "png"
    } else if data.starts_with(b"\xff\xd8") {
        "jpg"
    } else if data.starts_with(b"GIF89a") || data.starts_with(b"GIF87a") {
        "gif"
    } else if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP") {
        "webp"
    } else if data.starts_with(b"OggS") {
        "ogg"
    } else if data.len() > 8 && &data[4..8] == b"ftyp" {
        "m4a"
    } else {
        "bin"
    }
}
