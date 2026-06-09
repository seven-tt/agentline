use crate::error::Error;
use crate::types::{ChallengeResp, EventCallback, EventMessage, ImageContent, TextContent};
use agentline_bridge::types::{InboundMessage, MessageKind, PeerRef};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct WebhookConfig {
    pub verification_token: String,
    pub encrypt_key: String,
    pub allowed_users: Vec<String>,
}

#[derive(Clone)]
struct WebhookState {
    cfg: WebhookConfig,
    tx: mpsc::Sender<InboundMessage>,
}

pub fn spawn_webhook(
    bind: String,
    cfg: WebhookConfig,
    buffer: usize,
) -> (mpsc::Receiver<InboundMessage>, tokio::task::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(buffer);
    let state = Arc::new(WebhookState { cfg, tx });
    let app = Router::new()
        .route("/", post(handle_event))
        .with_state(state);

    let handle = tokio::spawn(async move {
        let listener = match tokio::net::TcpListener::bind(&bind).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(error=%e, bind=%bind, "feishu webhook bind failed");
                return;
            }
        };
        tracing::info!(bind=%bind, "feishu webhook server started");
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error=%e, "feishu webhook server error");
        }
    });

    (rx, handle)
}

async fn handle_event(
    State(state): State<Arc<WebhookState>>,
    Json(payload): Json<EventCallback>,
) -> impl IntoResponse {
    // URL verification challenge
    if payload.is_url_verification()
        && let Some(challenge) = payload.challenge
    {
        if !state.cfg.verification_token.is_empty()
            && let Some(ref token) = payload.token
            && token != &state.cfg.verification_token
        {
            tracing::warn!("feishu challenge: token mismatch");
            return (StatusCode::FORBIDDEN, Json(serde_json::json!({})));
        }
        return (
            StatusCode::OK,
            Json(serde_json::to_value(ChallengeResp { challenge }).unwrap()),
        );
    }

    // Verify event token
    if let Some(ref header) = payload.header
        && !state.cfg.verification_token.is_empty()
        && header.token != state.cfg.verification_token
    {
        tracing::warn!(event_id=%header.event_id, "feishu event: token mismatch");
        return (StatusCode::FORBIDDEN, Json(serde_json::json!({})));
    }

    // Only handle im.message.receive_v1
    let is_message_event = payload
        .header
        .as_ref()
        .is_some_and(|h| h.event_type == "im.message.receive_v1");
    if !is_message_event {
        return (StatusCode::OK, Json(serde_json::json!({})));
    }

    if let Some(event) = payload.event
        && let Err(e) = dispatch_message(&state, event.sender, event.message).await
    {
        tracing::warn!(error=%e, "feishu dispatch message failed");
    }

    (StatusCode::OK, Json(serde_json::json!({})))
}

async fn dispatch_message(
    state: &WebhookState,
    sender: Option<crate::types::EventSender>,
    message: Option<EventMessage>,
) -> Result<(), Error> {
    let sender = sender.ok_or_else(|| Error::Parse("missing sender".into()))?;
    let sender_id = sender
        .sender_id
        .ok_or_else(|| Error::Parse("missing sender_id".into()))?;
    let msg = message.ok_or_else(|| Error::Parse("missing message".into()))?;

    // allowed_users filter
    if !state.cfg.allowed_users.is_empty() && !state.cfg.allowed_users.contains(&sender_id.open_id)
    {
        tracing::debug!(open_id=%sender_id.open_id, "feishu: user not in allowed_users, ignoring");
        return Ok(());
    }

    let peer = PeerRef {
        user_id: sender_id.open_id.clone(),
        group_id: if msg.chat_type == "group" {
            Some(msg.chat_id.clone())
        } else {
            None
        },
        opaque: serde_json::json!({
            "message_id": msg.message_id,
            "chat_id": msg.chat_id,
            "chat_type": msg.chat_type,
        }),
    };

    let kind = parse_message_kind(&msg)?;

    let inbound = InboundMessage {
        peer,
        kind,
        received_at: std::time::SystemTime::now(),
    };

    state
        .tx
        .send(inbound)
        .await
        .map_err(|_| Error::Other("inbound channel closed".into()))?;
    Ok(())
}

fn parse_message_kind(msg: &EventMessage) -> Result<MessageKind, Error> {
    match msg.message_type.as_str() {
        "text" => {
            let content: TextContent = serde_json::from_str(&msg.content)
                .map_err(|e| Error::Parse(format!("text content: {e}")))?;
            Ok(MessageKind::Text { text: content.text })
        }
        "image" => {
            let _content: ImageContent = serde_json::from_str(&msg.content)
                .map_err(|e| Error::Parse(format!("image content: {e}")))?;
            Ok(MessageKind::Image {
                local_path: None,
                caption: None,
            })
        }
        other => {
            tracing::debug!(msg_type=%other, "feishu: unsupported message type, treating as text");
            Ok(MessageKind::Text {
                text: format!("[unsupported: {other}]"),
            })
        }
    }
}
