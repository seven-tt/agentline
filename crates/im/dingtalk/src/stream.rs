//! WebSocket stream loop. Connects to the Dingtalk gateway, decodes
//! `DataFrame`s, ACKs, dispatches CALLBACK messages to the bridge, and
//! reconnects with backoff on disconnect.

use crate::auth::{OpenParams, TOPIC_BOT_MESSAGE, open_connection};
use crate::error::{Error, Result};
use crate::types::{BotCallback, DataFrame, DataFrameResponse};
use agentline_im_core::types::{InboundMessage, MessageKind, PeerRef};
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

const KEEPALIVE_IDLE: Duration = Duration::from_secs(120);
const RECONNECT_MIN: Duration = Duration::from_secs(3);
const RECONNECT_MAX: Duration = Duration::from_secs(60);

/// Holds the current `sessionWebhook` for the active session, if any. The
/// outbound side reads it when sending a reply.
///
/// We keep ONE webhook per (peer.user_id) at any time — DingTalk gives a
/// fresh one with every inbound message, so we always have the most recent
/// from the user's last turn.
pub type WebhookCache = Arc<Mutex<std::collections::HashMap<String, String>>>;

pub struct StreamConfig {
    pub open: OpenParams,
    pub allowed_users: Vec<String>,
    pub buffer: usize,
}

/// Starts the long-lived stream loop. Returns the inbound mpsc receiver and
/// a shared webhook cache.
pub fn spawn_stream(
    cfg: StreamConfig,
) -> (mpsc::Receiver<InboundMessage>, WebhookCache, JoinHandle<()>) {
    let webhooks: WebhookCache = Arc::new(Mutex::new(std::collections::HashMap::new()));
    let (rx, handle) = spawn_stream_with(cfg, webhooks.clone());
    (rx, webhooks, handle)
}

/// Like [`spawn_stream`] but uses an existing [`WebhookCache`].
pub fn spawn_stream_with(
    cfg: StreamConfig,
    webhooks: WebhookCache,
) -> (mpsc::Receiver<InboundMessage>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(cfg.buffer.max(1));
    let webhooks_for_task = webhooks.clone();
    let handle = tokio::spawn(async move {
        run_loop(cfg, tx, webhooks_for_task).await;
    });
    (rx, handle)
}

async fn run_loop(cfg: StreamConfig, tx: mpsc::Sender<InboundMessage>, webhooks: WebhookCache) {
    let mut backoff = RECONNECT_MIN;
    loop {
        match run_once(&cfg, &tx, &webhooks).await {
            Ok(()) => {
                tracing::info!("dingtalk: stream connection closed cleanly; reconnecting");
                backoff = RECONNECT_MIN;
            }
            Err(e) => {
                tracing::warn!(error=%e, "dingtalk: stream errored; reconnecting after {backoff:?}");
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(RECONNECT_MAX);
    }
}

async fn run_once(
    cfg: &StreamConfig,
    tx: &mpsc::Sender<InboundMessage>,
    webhooks: &WebhookCache,
) -> Result<()> {
    let endpoint = open_connection(&cfg.open).await?;
    let url = format!("{}?ticket={}", endpoint.endpoint, endpoint.ticket);
    tracing::info!("dingtalk: connecting to stream gateway");
    let (ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .map_err(|e| Error::ws(format!("connect {url}: {e}")))?;

    let (mut write, mut read) = ws.split();
    let mut idle = tokio::time::interval(KEEPALIVE_IDLE);
    idle.tick().await; // discard immediate

    loop {
        tokio::select! {
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        let frame: DataFrame = match serde_json::from_str(&text) {
                            Ok(f) => f,
                            Err(e) => {
                                tracing::warn!(error=%e, "dingtalk: bad frame");
                                continue;
                            }
                        };
                        // ACK every frame.
                        let ack = DataFrameResponse::ok(frame.message_id());
                        let body = serde_json::to_string(&ack)
                            .map_err(|e| Error::other(format!("encode ack: {e}")))?;
                        write
                            .send(Message::Text(body))
                            .await
                            .map_err(|e| Error::ws(format!("send ack: {e}")))?;
                        // Dispatch.
                        match frame.kind.as_str() {
                            "CALLBACK" if frame.topic() == TOPIC_BOT_MESSAGE => {
                                if let Err(e) = handle_callback(&frame.data, cfg, tx, webhooks).await {
                                    tracing::warn!(error=%e, "dingtalk: handle_callback failed");
                                }
                            }
                            "SYSTEM" if frame.topic() == "disconnect" => {
                                tracing::info!("dingtalk: server requested disconnect; reconnecting");
                                return Ok(());
                            }
                            "SYSTEM" if frame.topic() == "ping" => {
                                // ACK already sent above; nothing else to do.
                            }
                            _ => {
                                tracing::debug!(kind=%frame.kind, topic=%frame.topic(), "dingtalk: unhandled frame");
                            }
                        }
                    }
                    Some(Ok(Message::Pong(_))) => { /* swallow */ }
                    Some(Ok(Message::Ping(p))) => {
                        write
                            .send(Message::Pong(p))
                            .await
                            .map_err(|e| Error::ws(format!("send pong: {e}")))?;
                    }
                    Some(Ok(Message::Close(_))) => {
                        return Ok(());
                    }
                    Some(Ok(_)) => { /* binary, etc. — ignore */ }
                    Some(Err(e)) => {
                        return Err(Error::ws(format!("read: {e}")));
                    }
                    None => return Ok(()),
                }
                idle.reset();
            }
            _ = idle.tick() => {
                // Send a websocket-level ping; if peer doesn't pong in 5s the
                // next read will hit a timeout/close and we'll reconnect.
                if let Err(e) = write.send(Message::Ping(vec![])).await {
                    return Err(Error::ws(format!("send ping: {e}")));
                }
            }
        }
    }
}

async fn handle_callback(
    data: &str,
    cfg: &StreamConfig,
    tx: &mpsc::Sender<InboundMessage>,
    webhooks: &WebhookCache,
) -> Result<()> {
    let cb: BotCallback =
        serde_json::from_str(data).map_err(|e| Error::Parse(format!("bot callback: {e}")))?;

    if !cfg.allowed_users.is_empty() && !cfg.allowed_users.iter().any(|u| u == &cb.sender_staff_id)
    {
        tracing::debug!(user=%cb.sender_staff_id, "dingtalk: not in allow-list");
        return Ok(());
    }

    if !cb.session_webhook.is_empty() {
        webhooks
            .lock()
            .await
            .insert(cb.sender_staff_id.clone(), cb.session_webhook.clone());
    }

    // Currently only text messages are forwarded; richText / image / audio
    // need separate handling that we don't ship yet.
    let kind = if cb.msgtype == "text" {
        let content = cb
            .text
            .as_ref()
            .map(|t| t.content.clone())
            .unwrap_or_default();
        MessageKind::Text { text: content }
    } else {
        tracing::debug!(msgtype=%cb.msgtype, "dingtalk: unsupported msgtype");
        return Ok(());
    };

    let opaque = serde_json::json!({
        "session_webhook": cb.session_webhook,
        "msg_id": cb.msg_id,
        "conversation_id": cb.conversation_id,
    });
    let peer = PeerRef {
        user_id: cb.sender_staff_id.clone(),
        group_id: if cb.is_group() {
            Some(cb.conversation_id.clone())
        } else {
            None
        },
        opaque,
    };
    let msg = InboundMessage {
        peer,
        kind,
        received_at: SystemTime::now(),
    };
    tx.send(msg)
        .await
        .map_err(|_| Error::other("inbound channel closed"))?;
    Ok(())
}
