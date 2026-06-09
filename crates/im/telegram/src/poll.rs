use crate::error::Error;
use crate::types::{ApiResponse, Update};
use agentline_bridge::types::{InboundMessage, MessageKind, PeerRef};
use std::time::Duration;
use tokio::sync::mpsc;

const POLL_TIMEOUT: u64 = 30;

pub fn spawn_poll(
    http: reqwest::Client,
    api_base: String,
    token: String,
    allowed_users: Vec<String>,
    buffer: usize,
) -> (mpsc::Receiver<InboundMessage>, tokio::task::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(buffer);
    let handle = tokio::spawn(async move {
        let mut offset: Option<i64> = None;
        loop {
            match poll_once(&http, &api_base, &token, offset).await {
                Ok(updates) => {
                    for update in updates {
                        if let Some(new_offset) = process_update(&tx, &update, &allowed_users).await
                        {
                            offset = Some(new_offset);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error=%e, "telegram getUpdates failed; retrying in 5s");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    });
    (rx, handle)
}

async fn poll_once(
    http: &reqwest::Client,
    api_base: &str,
    token: &str,
    offset: Option<i64>,
) -> Result<Vec<Update>, Error> {
    let url = format!("{api_base}/bot{token}/getUpdates");
    let mut params: Vec<(&str, String)> = vec![("timeout", POLL_TIMEOUT.to_string())];
    if let Some(off) = offset {
        params.push(("offset", off.to_string()));
    }

    let resp = http
        .get(&url)
        .query(&params)
        .timeout(Duration::from_secs(POLL_TIMEOUT + 10))
        .send()
        .await
        .map_err(|e| Error::http(format!("getUpdates: {e}")))?;

    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| Error::http(format!("read body: {e}")))?;

    if !status.is_success() {
        return Err(Error::Api(format!(
            "getUpdates → {}: {}",
            status,
            String::from_utf8_lossy(&bytes)
        )));
    }

    let parsed: ApiResponse<Vec<Update>> =
        serde_json::from_slice(&bytes).map_err(|e| Error::Parse(format!("getUpdates: {e}")))?;

    if !parsed.ok {
        return Err(Error::Api(
            parsed.description.unwrap_or_else(|| "unknown error".into()),
        ));
    }

    Ok(parsed.result.unwrap_or_default())
}

async fn process_update(
    tx: &mpsc::Sender<InboundMessage>,
    update: &Update,
    allowed_users: &[String],
) -> Option<i64> {
    let new_offset = update.update_id + 1;

    let message = match &update.message {
        Some(m) => m,
        None => return Some(new_offset),
    };

    let user = match &message.from {
        Some(u) => u,
        None => return Some(new_offset),
    };

    let user_id_str = user.id.to_string();
    if !allowed_users.is_empty() && !allowed_users.contains(&user_id_str) {
        tracing::debug!(user_id=%user.id, "telegram: user not in allowed_users, ignoring");
        return Some(new_offset);
    }

    let kind = parse_message_kind(message);
    let peer = PeerRef {
        user_id: user_id_str,
        group_id: if message.chat.chat_type != "private" {
            Some(message.chat.id.to_string())
        } else {
            None
        },
        opaque: serde_json::json!({
            "chat_id": message.chat.id,
            "message_id": message.message_id,
        }),
    };

    let inbound = InboundMessage {
        peer,
        kind,
        received_at: std::time::SystemTime::now(),
    };

    if tx.send(inbound).await.is_err() {
        tracing::error!("telegram: inbound channel closed");
    }

    Some(new_offset)
}

fn parse_message_kind(msg: &crate::types::Message) -> MessageKind {
    if let Some(ref text) = msg.text {
        return MessageKind::Text { text: text.clone() };
    }
    if msg.photo.is_some() {
        return MessageKind::Image {
            local_path: None,
            caption: msg.caption.clone(),
        };
    }
    if msg.voice.is_some() {
        return MessageKind::Voice {
            transcript: None,
            local_path: None,
        };
    }
    if msg.video.is_some() {
        return MessageKind::Video {
            local_path: None,
            caption: msg.caption.clone(),
        };
    }
    if let Some(ref doc) = msg.document {
        return MessageKind::File {
            local_path: std::path::PathBuf::new(),
            name: doc.file_name.clone().unwrap_or_else(|| "file".into()),
        };
    }
    MessageKind::Text {
        text: "[unsupported message type]".into(),
    }
}
