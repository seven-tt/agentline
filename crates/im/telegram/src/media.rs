//! Media download for Telegram inbound photos/voice/video/documents via the
//! Bot API's `getFile` + file CDN endpoint.

use crate::types::{ApiResponse, FileInfo};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub fn media_save_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".agentline")
        .join("media")
        .join("inbound")
}

/// Resolves `file_id` to a CDN path via `getFile`, downloads it, and saves it
/// under `save_dir`. `original_name` (if known, e.g. a document's filename)
/// takes priority for the saved extension; otherwise it's inferred from the
/// CDN-reported `file_path`.
pub async fn download_file(
    http: &reqwest::Client,
    api_base: &str,
    token: &str,
    file_id: &str,
    prefix: &str,
    original_name: Option<&str>,
    save_dir: &Path,
) -> Option<PathBuf> {
    let remote_path = get_file_path(http, api_base, token, file_id).await?;

    let url = format!("{api_base}/file/bot{token}/{remote_path}");
    let resp = http
        .get(&url)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .map_err(|e| tracing::error!("telegram media download failed: {e}"))
        .ok()?;

    if !resp.status().is_success() {
        tracing::error!(
            status = %resp.status(),
            "telegram media download failed for file_id={file_id}"
        );
        return None;
    }

    let data = resp
        .bytes()
        .await
        .map_err(|e| tracing::error!("telegram media read body failed: {e}"))
        .ok()?;

    let ext = original_name
        .and_then(|n| Path::new(n).extension())
        .or_else(|| Path::new(&remote_path).extension())
        .and_then(|e| e.to_str())
        .unwrap_or("bin");
    let filename = format!("{prefix}_{}.{ext}", ulid::Ulid::new());
    let path = save_dir.join(&filename);

    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Err(e) = tokio::fs::write(&path, &data).await {
        tracing::error!("telegram media write failed: {e}");
        return None;
    }
    tracing::debug!("telegram media saved: {}", path.display());
    Some(path)
}

async fn get_file_path(
    http: &reqwest::Client,
    api_base: &str,
    token: &str,
    file_id: &str,
) -> Option<String> {
    let url = format!("{api_base}/bot{token}/getFile");
    let resp = http
        .get(&url)
        .query(&[("file_id", file_id)])
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| tracing::error!("telegram getFile failed: {e}"))
        .ok()?;

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| tracing::error!("telegram getFile read body failed: {e}"))
        .ok()?;

    let parsed: ApiResponse<FileInfo> = serde_json::from_slice(&bytes)
        .map_err(|e| tracing::error!("telegram getFile parse failed: {e}"))
        .ok()?;

    if !parsed.ok {
        tracing::error!(
            description = ?parsed.description,
            "telegram getFile returned not-ok for file_id={file_id}"
        );
        return None;
    }

    parsed.result.and_then(|f| f.file_path)
}
