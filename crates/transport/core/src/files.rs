//! File browsing protocol handler.
//!
//! Serves JSON-RPC-style requests over a transport connection:
//! - `list_changed` — git-tracked changes (M/A/D/…)
//! - `list_files`   — recursive directory listing
//! - `read_file`    — single file content
//! - `file_diff`    — git diff for a single file

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::TransportConn;

const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10 MB

// ── Request / Response ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct Request {
    method: String,
    #[serde(default)]
    path: String,
}

#[derive(Debug, Serialize)]
struct ChangedFilesResponse {
    files: Vec<ChangedFile>,
}

#[derive(Debug, Serialize)]
struct ChangedFile {
    path: String,
    status: String,
}

#[derive(Debug, Serialize)]
struct ListFilesResponse {
    entries: Vec<FileEntry>,
}

#[derive(Debug, Serialize)]
struct FileEntry {
    path: String,
    is_dir: bool,
}

#[derive(Debug, Serialize)]
struct ContentResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

// ── Connection handler ─────────────────────────────────────────────────

pub async fn handle_files_connection(
    cwd: PathBuf,
    conn_id: u64,
    conn: TransportConn,
    token: Option<&str>,
) {
    let source_id = format!("files:{conn_id}");

    let conn = if let Some(token) = token {
        match crate::auth::wrap_authenticated(token, conn).await {
            Ok(c) => {
                tracing::info!(source=%source_id, "files connection authenticated");
                c
            }
            Err(e) => {
                tracing::warn!(source=%source_id, error=%e, "files auth rejected");
                return;
            }
        }
    } else {
        tracing::debug!(source=%source_id, "files connection accepted (no auth)");
        conn
    };

    let mut reader = BufReader::new(conn.read);
    let mut writer = conn.write;

    let mut line = String::new();
    match reader.read_line(&mut line).await {
        Ok(0) => return,
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(source=%source_id, error=%e, "files read request failed");
            return;
        }
    }

    let response = match serde_json::from_str::<Request>(line.trim()) {
        Ok(req) => dispatch(&cwd, &req).await,
        Err(e) => serde_json::to_string(&ErrorResponse {
            error: format!("bad request: {e}"),
        })
        .unwrap_or_default(),
    };

    let mut out = response.into_bytes();
    out.push(b'\n');
    let _ = writer.write_all(&out).await;
    let _ = writer.flush().await;
    let _ = writer.shutdown().await;
}

async fn dispatch(cwd: &Path, req: &Request) -> String {
    let result = match req.method.as_str() {
        "list_changed" => do_list_changed(cwd).await,
        "list_files" => do_list_files(cwd, &req.path),
        "read_file" => do_read_file(cwd, &req.path).await,
        "file_diff" => do_file_diff(cwd, &req.path).await,
        other => Err(format!("unknown method: {other}")),
    };
    match result {
        Ok(json) => json,
        Err(e) => serde_json::to_string(&ErrorResponse { error: e }).unwrap_or_default(),
    }
}

// ── Path safety ────────────────────────────────────────────────────────

fn safe_resolve(cwd: &Path, relative: &str) -> Result<PathBuf, String> {
    if relative.is_empty() || relative == "." {
        return Ok(cwd.to_path_buf());
    }
    let joined = cwd.join(relative);
    let resolved = joined
        .canonicalize()
        .map_err(|e| format!("path not found: {e}"))?;
    let cwd_canonical = cwd
        .canonicalize()
        .map_err(|e| format!("cwd resolve error: {e}"))?;
    if !resolved.starts_with(&cwd_canonical) {
        return Err("path traversal denied".into());
    }
    if resolved.components().any(|c| c.as_os_str() == ".git") {
        return Err("access to .git denied".into());
    }
    Ok(resolved)
}

// ── Method implementations ─────────────────────────────────────────────

async fn do_list_changed(cwd: &Path) -> Result<String, String> {
    let output = tokio::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(cwd)
        .output()
        .await
        .map_err(|e| format!("git status failed: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git status error: {stderr}"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let files: Vec<ChangedFile> = stdout
        .lines()
        .filter(|line| line.len() >= 4)
        .map(|line| {
            let status = line[..2].trim().to_string();
            let path = line[3..].to_string();
            ChangedFile { path, status }
        })
        .collect();

    serde_json::to_string(&ChangedFilesResponse { files })
        .map_err(|e| format!("serialize error: {e}"))
}

fn do_list_files(cwd: &Path, relative: &str) -> Result<String, String> {
    let target = safe_resolve(cwd, relative)?;
    let mut entries = Vec::new();

    let cwd_canonical = cwd
        .canonicalize()
        .map_err(|e| format!("cwd resolve error: {e}"))?;

    collect_entries(&cwd_canonical, &target, &mut entries)?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));

    serde_json::to_string(&ListFilesResponse { entries })
        .map_err(|e| format!("serialize error: {e}"))
}

fn collect_entries(cwd: &Path, dir: &Path, entries: &mut Vec<FileEntry>) -> Result<(), String> {
    let read = std::fs::read_dir(dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
    for entry in read.flatten() {
        let path = entry.path();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if name.starts_with('.') {
            continue;
        }
        let is_dir = path.is_dir();
        let rel = path
            .strip_prefix(cwd)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        entries.push(FileEntry { path: rel, is_dir });
        if is_dir {
            collect_entries(cwd, &path, entries)?;
        }
    }
    Ok(())
}

async fn do_read_file(cwd: &Path, relative: &str) -> Result<String, String> {
    if relative.is_empty() {
        return Err("path is required".into());
    }
    let target = safe_resolve(cwd, relative)?;
    if target.is_dir() {
        return Err("path is a directory".into());
    }

    let meta = tokio::fs::metadata(&target)
        .await
        .map_err(|e| format!("stat error: {e}"))?;
    if meta.len() > MAX_FILE_SIZE {
        return Err(format!(
            "file too large: {} bytes (max {})",
            meta.len(),
            MAX_FILE_SIZE
        ));
    }

    let content = tokio::fs::read_to_string(&target)
        .await
        .map_err(|e| format!("read error: {e}"))?;

    serde_json::to_string(&ContentResponse {
        content: Some(content),
        diff: None,
        error: None,
    })
    .map_err(|e| format!("serialize error: {e}"))
}

async fn do_file_diff(cwd: &Path, relative: &str) -> Result<String, String> {
    if relative.is_empty() {
        return Err("path is required".into());
    }
    // Validate path is within cwd (canonicalize may fail for new untracked files)
    let joined = cwd.join(relative);
    if let Ok(resolved) = joined.canonicalize() {
        let cwd_canonical = cwd
            .canonicalize()
            .map_err(|e| format!("cwd resolve error: {e}"))?;
        if !resolved.starts_with(&cwd_canonical) {
            return Err("path traversal denied".into());
        }
    }

    let output = tokio::process::Command::new("git")
        .args(["diff", relative])
        .current_dir(cwd)
        .output()
        .await
        .map_err(|e| format!("git diff failed: {e}"))?;

    let diff = String::from_utf8_lossy(&output.stdout).to_string();

    serde_json::to_string(&ContentResponse {
        content: None,
        diff: Some(diff),
        error: None,
    })
    .map_err(|e| format!("serialize error: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_request() {
        let req: Request = serde_json::from_str(r#"{"method":"list_changed"}"#).unwrap();
        assert_eq!(req.method, "list_changed");
        assert_eq!(req.path, "");

        let req: Request =
            serde_json::from_str(r#"{"method":"read_file","path":"src/main.rs"}"#).unwrap();
        assert_eq!(req.method, "read_file");
        assert_eq!(req.path, "src/main.rs");
    }

    #[test]
    fn path_traversal_denied() {
        let cwd = std::env::current_dir().unwrap();
        let result = safe_resolve(&cwd, "../../etc/passwd");
        assert!(result.is_err());
    }

    #[test]
    fn dot_git_denied() {
        let cwd = std::env::current_dir().unwrap();
        let result = safe_resolve(&cwd, ".git/config");
        assert!(result.is_err());
    }

    #[test]
    fn serialize_changed_files() {
        let resp = ChangedFilesResponse {
            files: vec![
                ChangedFile {
                    path: "src/main.rs".into(),
                    status: "M".into(),
                },
                ChangedFile {
                    path: "src/new.rs".into(),
                    status: "A".into(),
                },
            ],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""status":"M""#));
        assert!(json.contains(r#""status":"A""#));
    }

    #[test]
    fn serialize_error() {
        let resp = ErrorResponse {
            error: "not found".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""error":"not found""#));
    }
}
