use crate::error::Error;
use crate::send;
use crate::types::{ApiResponse, Update};
use agentline_im_core::parse_inbound;
use agentline_im_core::types::{InboundMessage, MessageKind, PeerRef, SourceMessage};
use std::time::Duration;
use tokio::sync::mpsc;

const POLL_TIMEOUT: u64 = 30;

pub fn spawn_poll(
    http: reqwest::Client,
    api_base: String,
    token: String,
    allowed_users: Vec<String>,
    buffer: usize,
) -> (mpsc::Receiver<SourceMessage>, tokio::task::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(buffer);
    let bot_id = token.split(':').next().unwrap_or_default().to_string();
    let handle = tokio::spawn(async move {
        tracing::info!(bot_id = %bot_id, "telegram: polling started");
        let mut offset: Option<i64> = None;
        loop {
            match poll_once(&http, &api_base, &token, offset).await {
                Ok(updates) => {
                    for update in updates {
                        if let Some(new_offset) =
                            process_update(&http, &api_base, &token, &tx, &update, &allowed_users)
                                .await
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
    http: &reqwest::Client,
    api_base: &str,
    token: &str,
    tx: &mpsc::Sender<SourceMessage>,
    update: &Update,
    allowed_users: &[String],
) -> Option<i64> {
    let new_offset = update.update_id + 1;

    // Handle callback queries (inline keyboard button presses)
    if let Some(cb) = &update.callback_query {
        let _ = send::answer_callback_query(http, api_base, token, &cb.id).await;
        if let Some(data) = &cb.data {
            let chat_id = cb.message.as_ref().map(|m| m.chat.id).unwrap_or(0);
            let user_id_str = cb.from.id.to_string();
            let text = match data.as_str() {
                "perm:y" => "y",
                "perm:n" => "n",
                "perm:s" => "s",
                _ => return Some(new_offset),
            };
            let peer = PeerRef {
                user_id: user_id_str,
                group_id: None,
                opaque: serde_json::json!({ "chat_id": chat_id }),
            };
            let inbound = InboundMessage {
                peer,
                kind: MessageKind::Text {
                    text: text.to_string(),
                },
                received_at: std::time::SystemTime::now(),
            };
            if tx.send(parse_inbound(inbound)).await.is_err() {
                tracing::error!("telegram: inbound channel closed");
            }
        }
        return Some(new_offset);
    }

    let message = match &update.message {
        Some(m) => m,
        None => return Some(new_offset),
    };

    let user = match &message.from {
        Some(u) => u,
        None => return Some(new_offset),
    };

    let user_id_str = user.id.to_string();
    tracing::info!(user_id = %user_id_str, "telegram: message from user");
    if !allowed_users.is_empty() && !allowed_users.contains(&user_id_str) {
        tracing::debug!(user_id = %user_id_str, "telegram: user not in allowed_users, ignoring");
        return Some(new_offset);
    }

    let kind = parse_message_kind(http, api_base, token, message).await;
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

    if tx.send(parse_inbound(inbound)).await.is_err() {
        tracing::error!("telegram: inbound channel closed");
    }

    Some(new_offset)
}

async fn parse_message_kind(
    http: &reqwest::Client,
    api_base: &str,
    token: &str,
    msg: &crate::types::Message,
) -> MessageKind {
    if let Some(ref text) = msg.text {
        return MessageKind::Text { text: text.clone() };
    }
    let save_dir = crate::media::media_save_dir();
    if let Some(largest) = msg.photo.as_ref().and_then(|p| p.last()) {
        let local_path = crate::media::download_file(
            http,
            api_base,
            token,
            &largest.file_id,
            "img",
            None,
            &save_dir,
        )
        .await;
        return MessageKind::Image {
            local_path,
            caption: msg.caption.clone(),
        };
    }
    if let Some(ref voice) = msg.voice {
        let local_path = crate::media::download_file(
            http,
            api_base,
            token,
            &voice.file_id,
            "voice",
            None,
            &save_dir,
        )
        .await;
        return MessageKind::Voice {
            transcript: None,
            local_path,
        };
    }
    if let Some(ref video) = msg.video {
        let local_path = crate::media::download_file(
            http,
            api_base,
            token,
            &video.file_id,
            "video",
            None,
            &save_dir,
        )
        .await;
        return MessageKind::Video {
            local_path,
            caption: msg.caption.clone(),
        };
    }
    if let Some(ref doc) = msg.document {
        let name = doc.file_name.clone().unwrap_or_else(|| "file".into());
        let local_path = crate::media::download_file(
            http,
            api_base,
            token,
            &doc.file_id,
            "file",
            Some(&name),
            &save_dir,
        )
        .await
        .unwrap_or_default();
        return MessageKind::File { local_path, name };
    }
    MessageKind::Text {
        text: "[unsupported message type]".into(),
    }
}
