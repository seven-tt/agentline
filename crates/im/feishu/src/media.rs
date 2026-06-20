//! Media download for Feishu inbound images and files.

use crate::auth::TokenManager;
use std::path::{Path, PathBuf};
use std::time::Duration;

const IMAGE_URL_BASE: &str = "https://open.feishu.cn/open-apis/im/v1/images";
const MSG_URL_BASE: &str = "https://open.feishu.cn/open-apis/im/v1/messages";

pub fn media_save_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".agentline")
        .join("media")
        .join("inbound")
}

pub async fn download_image(
    http: &reqwest::Client,
    token_mgr: &TokenManager,
    image_key: &str,
    save_dir: &Path,
) -> Option<PathBuf> {
    let url = format!("{IMAGE_URL_BASE}/{image_key}");
    let token = token_mgr.token().await;
    let resp = http
        .get(&url)
        .timeout(Duration::from_secs(30))
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|e| tracing::error!("feishu image download failed: {e}"))
        .ok()?;

    if !resp.status().is_success() {
        tracing::error!(
            status = %resp.status(),
            "feishu image download failed for {image_key}"
        );
        return None;
    }

    let data = resp
        .bytes()
        .await
        .map_err(|e| tracing::error!("feishu image read body failed: {e}"))
        .ok()?;

    let ext = infer_image_ext(&data);
    let filename = format!("img_{}.{}", ulid::Ulid::new(), ext);
    let path = save_dir.join(&filename);

    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Err(e) = tokio::fs::write(&path, &data).await {
        tracing::error!("feishu image write failed: {e}");
        return None;
    }
    tracing::debug!("feishu image saved: {}", path.display());
    Some(path)
}

pub async fn download_file(
    http: &reqwest::Client,
    token_mgr: &TokenManager,
    message_id: &str,
    file_key: &str,
    file_name: &str,
    save_dir: &Path,
) -> Option<PathBuf> {
    let url = format!("{MSG_URL_BASE}/{message_id}/resources/{file_key}?type=file");
    let token = token_mgr.token().await;
    let resp = http
        .get(&url)
        .timeout(Duration::from_secs(60))
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|e| tracing::error!("feishu file download failed: {e}"))
        .ok()?;

    if !resp.status().is_success() {
        tracing::error!(
            status = %resp.status(),
            "feishu file download failed for {file_key}"
        );
        return None;
    }

    let data = resp
        .bytes()
        .await
        .map_err(|e| tracing::error!("feishu file read body failed: {e}"))
        .ok()?;

    let ext = Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin");
    let filename = format!("file_{}.{}", ulid::Ulid::new(), ext);
    let path = save_dir.join(&filename);

    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Err(e) = tokio::fs::write(&path, &data).await {
        tracing::error!("feishu file write failed: {e}");
        return None;
    }
    tracing::debug!(
        "feishu file saved: {} (original: {})",
        path.display(),
        file_name
    );
    Some(path)
}

fn infer_image_ext(data: &[u8]) -> &'static str {
    if data.starts_with(b"\x89PNG") {
        "png"
    } else if data.starts_with(b"\xff\xd8") {
        "jpg"
    } else if data.starts_with(b"GIF89a") || data.starts_with(b"GIF87a") {
        "gif"
    } else if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP") {
        "webp"
    } else {
        "bin"
    }
}
