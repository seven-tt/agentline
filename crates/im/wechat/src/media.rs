//! Media download + AES-ECB decrypt for iLink inbound images/voice/video/file.

use crate::error::{Error, Result};
use crate::http::HttpClient;
use crate::types::{FileItem, ImageItem, MediaRef, VideoItem};
use aes::cipher::{BlockDecryptMut, KeyInit};
use aes::Aes128;
use base64::Engine;
use std::path::PathBuf;

const DEFAULT_CDN_BASE: &str = "https://novac2c.cdn.weixin.qq.com/c2c";

/// Decrypt AES-128-ECB with PKCS7 padding.
fn decrypt_aes_ecb(key: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
    let mut cipher = Aes128::new_from_slice(key)
        .map_err(|e| Error::other(format!("invalid AES key: {e}")))?;

    let mut buf = ciphertext.to_vec();

    // ECB mode: decrypt each 16-byte block independently.
    for chunk in buf.chunks_mut(16) {
        let mut block = aes::cipher::generic_array::GenericArray::clone_from_slice(chunk);
        cipher.decrypt_block_mut(&mut block);
        chunk.copy_from_slice(&block);
    }

    // PKCS7 unpadding.
    let pad_len = buf.last().copied().unwrap_or(0) as usize;
    if pad_len == 0 || pad_len > 16 || buf.len() < pad_len {
        return Err(Error::other("invalid PKCS7 padding".to_string()));
    }
    for i in 1..=pad_len {
        if buf[buf.len() - i] != pad_len as u8 {
            return Err(Error::other("invalid PKCS7 padding".to_string()));
        }
    }
    buf.truncate(buf.len() - pad_len);
    Ok(buf)
}

/// Parse CDNMedia.aes_key into a raw 16-byte AES key.
///
/// Two encodings are seen in the wild:
///   - base64(raw 16 bytes)          → images
///   - base64(hex string of 16 bytes) → file / voice / video
///
/// In the second case, base64-decoding yields 32 ASCII hex chars which must
/// then be parsed as hex to recover the actual 16-byte key.
fn parse_aes_key(aes_key_b64: &str, label: &str) -> Result<Vec<u8>> {
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(aes_key_b64)
        .map_err(|e| Error::other(format!("base64 decode aes_key: {e}")))?;

    if decoded.len() == 16 {
        return Ok(decoded);
    }
    if decoded.len() == 32 {
        let hex_str = String::from_utf8(decoded.clone())
            .map_err(|_| Error::other("aes_key decoded bytes are not valid UTF-8".to_string()))?;
        if hex_str.chars().all(|c| c.is_ascii_hexdigit()) {
            return hex::decode(&hex_str)
                .map_err(|e| Error::other(format!("hex decode aes_key: {e}")));
        }
    }
    Err(Error::other(format!(
        "{label}: aes_key must decode to 16 raw bytes or 32-char hex string, got {} bytes (base64=\"{aes_key_b64}\")",
        decoded.len()
    )))
}

/// Download a CDN resource and optionally decrypt it.
pub async fn download_media(
    http: &HttpClient,
    media: &MediaRef,
    label: &str,
) -> Result<Vec<u8>> {
    let cdn_base = DEFAULT_CDN_BASE;
    let url = format!(
        "{}/download?encrypted_query_param={}",
        cdn_base, media.encrypt_query_param
    );
    let bytes = http.get_bytes(&url).await?.to_vec();

    if let Some(ref aes_key_b64) = media.aes_key {
        let key = parse_aes_key(aes_key_b64, label)?;
        tracing::debug!("{label}: aes_key parsed to {} bytes", key.len());
        let pt = decrypt_aes_ecb(&key, &bytes)?;
        return Ok(pt);
    }

    tracing::debug!("{label}: no aes_key, returning plain bytes");
    Ok(bytes)
}

/// Resolve the AES key for an image item.
/// Priority: image_item.aeskey (hex) → image_item.media.aes_key (base64)
fn resolve_image_aes_key(img: &ImageItem) -> Option<String> {
    if let Some(ref hex_key) = img.aeskey {
        let bytes = match hex::decode(hex_key) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("invalid hex aeskey: {e}");
                return None;
            }
        };
        return Some(base64::engine::general_purpose::STANDARD.encode(bytes));
    }
    img.media.as_ref().and_then(|m| m.aes_key.clone())
}

/// Download an image, decrypt if needed, save to disk, return the path.
pub async fn download_image(
    http: &HttpClient,
    img: &ImageItem,
    save_dir: &PathBuf,
) -> Option<PathBuf> {
    let media = img.media.as_ref()?;
    let label = "image";

    let mut media_clone = media.clone();
    if media_clone.aes_key.is_none() {
        if let Some(key) = resolve_image_aes_key(img) {
            media_clone.aes_key = Some(key);
        }
    }

    let data = match download_media(http, &media_clone, label).await {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("image download failed: {e}");
            return None;
        }
    };

    let ext = infer_image_ext(&data);
    let filename = format!("img_{}.{}", ulid::Ulid::new(), ext);

    let path = save_dir.join(&filename);
    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Err(e) = tokio::fs::write(&path, &data).await {
        tracing::error!("image write failed: {e}");
        return None;
    }
    tracing::debug!("image saved: {}", path.display());
    Some(path)
}

/// Download a file, decrypt if needed, save to disk, return the path.
pub async fn download_file(
    http: &HttpClient,
    file: &FileItem,
    save_dir: &PathBuf,
) -> Option<PathBuf> {
    let media = file.media.as_ref()?;

    let data = match download_media(http, media, "file").await {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("file download failed: {e}");
            return None;
        }
    };

    let ext = file
        .file_name
        .as_ref()
        .and_then(|n| std::path::Path::new(n).extension())
        .and_then(|e| e.to_str())
        .unwrap_or("bin");
    let filename = format!("file_{}.{}", ulid::Ulid::new(), ext);

    let path = save_dir.join(&filename);
    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Err(e) = tokio::fs::write(&path, &data).await {
        tracing::error!("file write failed: {e}");
        return None;
    }
    tracing::debug!("file saved: {}", path.display());
    Some(path)
}

/// Download a video, decrypt if needed, save to disk, return the path.
pub async fn download_video(
    http: &HttpClient,
    video: &VideoItem,
    save_dir: &PathBuf,
) -> Option<PathBuf> {
    let media = video.media.as_ref()?;

    let data = match download_media(http, media, "video").await {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("video download failed: {e}");
            return None;
        }
    };

    let ext = infer_video_ext(&data);
    let filename = format!("video_{}.{}", ulid::Ulid::new(), ext);

    let path = save_dir.join(&filename);
    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Err(e) = tokio::fs::write(&path, &data).await {
        tracing::error!("video write failed: {e}");
        return None;
    }
    tracing::debug!("video saved: {}", path.display());
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

fn infer_video_ext(data: &[u8]) -> &'static str {
    // MP4: ftyp box at offset 4, or various brand signatures
    if data.len() > 12 && &data[4..8] == b"ftyp" {
        "mp4"
    } else if data.starts_with(b"\x00\x00\x00") && data.get(4..8) == Some(b"ftyp") {
        "mp4"
    } else {
        "bin"
    }
}