//! WebSocket long-connection stream loop for Feishu.
//!
//! Protocol: POST /callback/ws/endpoint → WSS URL; protobuf-encoded
//! binary frames; application-level ping/pong via Control frames.

use crate::auth::TokenManager;
use crate::error::{Error, Result};
use crate::types::EventCallback;
use agentline_im_core::parse_inbound;
use agentline_im_core::types::{InboundMessage, MessageKind, PeerRef, SourceMessage};
use futures::{SinkExt, StreamExt};
use rust_i18n::t;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

const ENDPOINT_URL: &str = "https://open.feishu.cn/callback/ws/endpoint";
const RECONNECT_MIN: Duration = Duration::from_secs(3);
const RECONNECT_MAX: Duration = Duration::from_secs(120);
const DEFAULT_PING_INTERVAL: u64 = 120;
const PACKET_CACHE_TTL: Duration = Duration::from_secs(5);

// ─── handshake types ─────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
struct EndpointReq<'a> {
    #[serde(rename = "AppID")]
    app_id: &'a str,
    #[serde(rename = "AppSecret")]
    app_secret: &'a str,
}

#[derive(Debug, serde::Deserialize)]
struct EndpointResp {
    code: i32,
    #[serde(default)]
    msg: String,
    data: Option<EndpointData>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct EndpointData {
    #[serde(alias = "URL")]
    url: String,
    client_config: Option<ClientConfig>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
#[allow(dead_code)]
struct ClientConfig {
    #[serde(default)]
    reconnect_count: i32,
    #[serde(default)]
    reconnect_interval: i64,
    #[serde(default)]
    reconnect_nonce: i64,
    #[serde(default = "default_ping_interval")]
    ping_interval: u64,
}

fn default_ping_interval() -> u64 {
    DEFAULT_PING_INTERVAL
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            reconnect_count: -1,
            reconnect_interval: 120,
            reconnect_nonce: 50,
            ping_interval: default_ping_interval(),
        }
    }
}

// ─── protobuf frame codec ────────────────────────────────────
//
// Wire format matches the Go SDK's pbbp2.proto Frame message (proto2):
//   1: uint64 SeqID       (required)
//   2: uint64 LogID       (required)
//   3: int32  service     (required)
//   4: int32  method      (required) — 0=Control, 1=Data
//   5: repeated Header { 1: string key, 2: string value }
//   6: string payload_encoding
//   7: string payload_type
//   8: bytes  payload
//   9: string LogIDNew

const METHOD_CONTROL: i32 = 0;
const METHOD_DATA: i32 = 1;

#[derive(Debug, Clone, Default)]
struct Frame {
    seq_id: u64,
    log_id: u64,
    service: i32,
    method: i32,
    headers: Vec<(String, String)>,
    payload: Vec<u8>,
}

impl Frame {
    fn header(&self, key: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn header_int(&self, key: &str) -> usize {
        self.header(key).and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    fn decode(data: &[u8]) -> Result<Self> {
        let mut frame = Frame::default();
        let mut pos = 0;
        while pos < data.len() {
            let (tag, wire_type, new_pos) = decode_tag(data, pos)?;
            pos = new_pos;
            match (tag, wire_type) {
                (1, 0) => {
                    let (v, p) = decode_varint(data, pos)?;
                    frame.seq_id = v;
                    pos = p;
                }
                (2, 0) => {
                    let (v, p) = decode_varint(data, pos)?;
                    frame.log_id = v;
                    pos = p;
                }
                (3, 0) => {
                    let (v, p) = decode_varint(data, pos)?;
                    frame.service = v as i32;
                    pos = p;
                }
                (4, 0) => {
                    let (v, p) = decode_varint(data, pos)?;
                    frame.method = v as i32;
                    pos = p;
                }
                (5, 2) => {
                    let (sub, p) = decode_bytes(data, pos)?;
                    pos = p;
                    let header = decode_header(sub)?;
                    frame.headers.push(header);
                }
                (6, 2) => {
                    let (_, p) = decode_bytes(data, pos)?;
                    pos = p;
                }
                (7, 2) => {
                    let (_, p) = decode_bytes(data, pos)?;
                    pos = p;
                }
                (8, 2) => {
                    let (b, p) = decode_bytes(data, pos)?;
                    frame.payload = b.to_vec();
                    pos = p;
                }
                (9, 2) => {
                    let (_, p) = decode_bytes(data, pos)?;
                    pos = p;
                }
                (_, 0) => {
                    let (_, p) = decode_varint(data, pos)?;
                    pos = p;
                }
                (_, 2) => {
                    let (_, p) = decode_bytes(data, pos)?;
                    pos = p;
                }
                (_, 5) => {
                    pos += 4;
                }
                (_, 1) => {
                    pos += 8;
                }
                _ => {
                    return Err(Error::ws(format!(
                        "unknown wire type {wire_type} at tag {tag}"
                    )));
                }
            }
        }
        Ok(frame)
    }

    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(128);
        // proto2 required fields — always write, even when zero
        encode_varint_field(&mut buf, 1, self.seq_id);
        encode_varint_field(&mut buf, 2, self.log_id);
        encode_varint_field(&mut buf, 3, self.service as u64);
        encode_varint_field(&mut buf, 4, self.method as u64);
        for (k, v) in &self.headers {
            let mut hdr = Vec::new();
            encode_bytes_field(&mut hdr, 1, k.as_bytes());
            encode_bytes_field(&mut hdr, 2, v.as_bytes());
            encode_bytes_field(&mut buf, 5, &hdr);
        }
        if !self.payload.is_empty() {
            encode_bytes_field(&mut buf, 8, &self.payload);
        }
        buf
    }
}

fn decode_header(data: &[u8]) -> Result<(String, String)> {
    let mut key = String::new();
    let mut val = String::new();
    let mut pos = 0;
    while pos < data.len() {
        let (tag, wt, new_pos) = decode_tag(data, pos)?;
        pos = new_pos;
        match (tag, wt) {
            (1, 2) => {
                let (b, p) = decode_bytes(data, pos)?;
                key = String::from_utf8_lossy(b).into_owned();
                pos = p;
            }
            (2, 2) => {
                let (b, p) = decode_bytes(data, pos)?;
                val = String::from_utf8_lossy(b).into_owned();
                pos = p;
            }
            (_, 0) => {
                let (_, p) = decode_varint(data, pos)?;
                pos = p;
            }
            (_, 2) => {
                let (_, p) = decode_bytes(data, pos)?;
                pos = p;
            }
            _ => {
                pos = data.len();
            }
        }
    }
    Ok((key, val))
}

fn decode_varint(data: &[u8], pos: usize) -> Result<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift = 0;
    let mut i = pos;
    loop {
        if i >= data.len() {
            return Err(Error::ws("truncated varint"));
        }
        let b = data[i];
        result |= ((b & 0x7f) as u64) << shift;
        i += 1;
        if b & 0x80 == 0 {
            return Ok((result, i));
        }
        shift += 7;
        if shift >= 64 {
            return Err(Error::ws("varint too large"));
        }
    }
}

fn decode_tag(data: &[u8], pos: usize) -> Result<(u32, u32, usize)> {
    let (v, new_pos) = decode_varint(data, pos)?;
    Ok(((v >> 3) as u32, (v & 0x7) as u32, new_pos))
}

fn decode_bytes(data: &[u8], pos: usize) -> Result<(&[u8], usize)> {
    let (len, new_pos) = decode_varint(data, pos)?;
    let len = len as usize;
    let end = new_pos + len;
    if end > data.len() {
        return Err(Error::ws("truncated length-delimited field"));
    }
    Ok((&data[new_pos..end], end))
}

fn encode_varint(buf: &mut Vec<u8>, mut v: u64) {
    loop {
        if v < 0x80 {
            buf.push(v as u8);
            return;
        }
        buf.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
}

fn encode_varint_field(buf: &mut Vec<u8>, field: u32, v: u64) {
    encode_varint(buf, (field as u64) << 3);
    encode_varint(buf, v);
}

fn encode_bytes_field(buf: &mut Vec<u8>, field: u32, data: &[u8]) {
    encode_varint(buf, ((field as u64) << 3) | 2);
    encode_varint(buf, data.len() as u64);
    buf.extend_from_slice(data);
}

// ─── multi-packet assembly ──────────────────────────────────

struct PacketEntry {
    slots: Vec<Option<Vec<u8>>>,
    created: Instant,
}

fn combine(
    cache: &mut HashMap<String, PacketEntry>,
    msg_id: &str,
    sum: usize,
    seq: usize,
    data: Vec<u8>,
) -> Option<Vec<u8>> {
    cache.retain(|_, e| e.created.elapsed() < PACKET_CACHE_TTL);

    let entry = cache
        .entry(msg_id.to_string())
        .or_insert_with(|| PacketEntry {
            slots: vec![None; sum],
            created: Instant::now(),
        });

    if seq < entry.slots.len() {
        entry.slots[seq] = Some(data);
    }

    if entry.slots.iter().all(|s| s.is_some()) {
        let full: Vec<u8> = entry
            .slots
            .iter()
            .flat_map(|s| s.as_ref().unwrap().iter().copied())
            .collect();
        cache.remove(msg_id);
        Some(full)
    } else {
        None
    }
}

// ─── public API ──────────────────────────────────────────────

pub struct WsConfig {
    pub app_id: String,
    pub app_secret: String,
    pub allowed_users: Vec<String>,
    pub buffer: usize,
    pub token_mgr: TokenManager,
    /// Shared with `FeishuChannel`: permission cards awaiting a click,
    /// keyed by the card's own message_id. Consumed (removed) on first
    /// click so a second click on the same card is a no-op instead of
    /// falling through as a stray chat message.
    pub perm_cards: Arc<Mutex<HashMap<String, crate::types::PermCardEntry>>>,
}

pub fn spawn_ws_stream(cfg: WsConfig) -> (mpsc::Receiver<SourceMessage>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(cfg.buffer.max(1));
    let handle = tokio::spawn(async move {
        run_loop(cfg, tx).await;
    });
    (rx, handle)
}

// ─── connection loop ─────────────────────────────────────────

async fn run_loop(cfg: WsConfig, tx: mpsc::Sender<SourceMessage>) {
    let http_for_media = reqwest::Client::builder()
        .build()
        .expect("build http client for media");
    let mut backoff = RECONNECT_MIN;
    loop {
        match run_once(&cfg, &http_for_media, &tx).await {
            Ok(()) => {
                tracing::info!("feishu: ws connection closed; reconnecting");
                backoff = RECONNECT_MIN;
            }
            Err(e) => {
                tracing::warn!(error=%e, "feishu: ws error; reconnecting after {backoff:?}");
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(RECONNECT_MAX);
    }
}

async fn open_endpoint(cfg: &WsConfig) -> Result<(String, ClientConfig, i32)> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| Error::http(format!("build http: {e}")))?;

    let body = EndpointReq {
        app_id: &cfg.app_id,
        app_secret: &cfg.app_secret,
    };
    let resp = http
        .post(ENDPOINT_URL)
        .header("locale", "zh")
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::http(format!("POST endpoint: {e}")))?;

    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| Error::http(format!("read body: {e}")))?;

    if !status.is_success() {
        return Err(Error::ws(format!(
            "endpoint → {}: {}",
            status,
            String::from_utf8_lossy(&bytes)
        )));
    }

    let parsed: EndpointResp = serde_json::from_slice(&bytes)
        .map_err(|e| Error::ws(format!("decode endpoint resp: {e}")))?;
    if parsed.code != 0 {
        return Err(Error::ws(format!(
            "endpoint code={}: {}",
            parsed.code, parsed.msg
        )));
    }

    let data = parsed
        .data
        .ok_or_else(|| Error::ws("endpoint response missing data"))?;
    let client_cfg = data.client_config.unwrap_or_default();

    let service_id = url::Url::parse(&data.url)
        .ok()
        .and_then(|u| {
            u.query_pairs()
                .find(|(k, _)| k == "service_id")
                .and_then(|(_, v)| v.parse::<i32>().ok())
        })
        .unwrap_or(0);

    Ok((data.url, client_cfg, service_id))
}

async fn run_once(
    cfg: &WsConfig,
    http_for_media: &reqwest::Client,
    tx: &mpsc::Sender<SourceMessage>,
) -> Result<()> {
    let (url, client_cfg, service_id) = open_endpoint(cfg).await?;
    tracing::info!(service_id, "feishu: connecting to ws gateway");

    let (ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .map_err(|e| Error::ws(format!("connect: {e}")))?;
    tracing::info!(app_id = %cfg.app_id, "feishu: ws connected");

    let (mut write, mut read) = ws.split();
    let mut ping_interval_secs = client_cfg.ping_interval;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(ping_interval_secs));
    heartbeat.tick().await; // discard immediate

    let mut packet_cache: HashMap<String, PacketEntry> = HashMap::new();

    loop {
        tokio::select! {
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        let frame = match Frame::decode(&data) {
                            Ok(f) => f,
                            Err(e) => {
                                tracing::warn!(error=%e, "feishu: bad binary frame");
                                continue;
                            }
                        };
                        match frame.method {
                            METHOD_CONTROL => {
                                match frame.header("type") {
                                    Some("ping") => {
                                        let pong = Frame {
                                            service: service_id,
                                            method: METHOD_CONTROL,
                                            headers: vec![("type".into(), "pong".into())],
                                            ..Default::default()
                                        };
                                        write
                                            .send(Message::Binary(pong.encode()))
                                            .await
                                            .map_err(|e| Error::ws(format!("send pong: {e}")))?;
                                    }
                                    Some("pong") => {
                                        if !frame.payload.is_empty()
                                            && let Ok(conf) = serde_json::from_slice::<ClientConfig>(&frame.payload)
                                            && conf.ping_interval > 0
                                        {
                                            ping_interval_secs = conf.ping_interval;
                                            heartbeat = tokio::time::interval(Duration::from_secs(ping_interval_secs));
                                            heartbeat.tick().await;
                                            tracing::debug!(ping_interval_secs, "feishu: ping interval updated from pong");
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            METHOD_DATA => {
                                let start = Instant::now();

                                let sum = frame.header_int("sum").max(1);
                                let seq = frame.header_int("seq");

                                let payload = if sum > 1 {
                                    let msg_id = frame.header("message_id").unwrap_or("").to_string();
                                    match combine(&mut packet_cache, &msg_id, sum, seq, frame.payload.clone()) {
                                        Some(full) => full,
                                        None => {
                                            send_ack(&mut write, &frame, start).await?;
                                            continue;
                                        }
                                    }
                                } else {
                                    frame.payload.clone()
                                };

                                if frame.header("type") == Some("event")
                                    && let Err(e) = handle_event(&payload, cfg, http_for_media, tx).await
                                {
                                    tracing::warn!(error=%e, "feishu: handle_event failed");
                                }

                                send_ack(&mut write, &frame, start).await?;
                            }
                            _ => {
                                tracing::debug!(method=frame.method, "feishu: unknown frame method");
                            }
                        }
                    }
                    Some(Ok(Message::Ping(p))) => {
                        write.send(Message::Pong(p))
                            .await
                            .map_err(|e| Error::ws(format!("send ws pong: {e}")))?;
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) => return Ok(()),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(Error::ws(format!("read: {e}"))),
                    None => return Ok(()),
                }
                heartbeat.reset();
            }
            _ = heartbeat.tick() => {
                let ping = Frame {
                    service: service_id,
                    method: METHOD_CONTROL,
                    headers: vec![("type".into(), "ping".into())],
                    ..Default::default()
                };
                write
                    .send(Message::Binary(ping.encode()))
                    .await
                    .map_err(|e| Error::ws(format!("send heartbeat ping: {e}")))?;
            }
        }
    }
}

async fn send_ack<S>(write: &mut S, frame: &Frame, start: Instant) -> Result<()>
where
    S: futures::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    let elapsed_ms = start.elapsed().as_millis();
    let resp = serde_json::json!({"code": 200});

    let mut ack_headers = frame.headers.clone();
    ack_headers.push(("biz_rt".into(), elapsed_ms.to_string()));

    let ack = Frame {
        seq_id: frame.seq_id,
        log_id: frame.log_id,
        service: frame.service,
        method: frame.method,
        headers: ack_headers,
        payload: resp.to_string().into_bytes(),
    };
    write
        .send(Message::Binary(ack.encode()))
        .await
        .map_err(|e| Error::ws(format!("send ack: {e}")))?;
    Ok(())
}

// ─── event dispatch ──────────────────────────────────────────

async fn handle_event(
    payload: &[u8],
    cfg: &WsConfig,
    http: &reqwest::Client,
    tx: &mpsc::Sender<SourceMessage>,
) -> Result<()> {
    let cb: EventCallback =
        serde_json::from_slice(payload).map_err(|e| Error::Parse(format!("event payload: {e}")))?;

    let event_type = cb
        .header
        .as_ref()
        .map(|h| h.event_type.as_str())
        .unwrap_or("");

    match event_type {
        "im.message.receive_v1" => {}
        "card.action.trigger" => {
            return handle_card_action(cb, cfg, http, tx).await;
        }
        _ => return Ok(()),
    }

    let event = match cb.event {
        Some(e) => e,
        None => return Ok(()),
    };

    let sender = event
        .sender
        .ok_or_else(|| Error::Parse("missing sender".into()))?;
    let sender_id = sender
        .sender_id
        .ok_or_else(|| Error::Parse("missing sender_id".into()))?;
    tracing::info!(user_id = %sender_id.open_id, "feishu: message from user");
    let msg = event
        .message
        .ok_or_else(|| Error::Parse("missing message".into()))?;

    if !cfg.allowed_users.is_empty() && !cfg.allowed_users.contains(&sender_id.open_id) {
        tracing::debug!(user_id = %sender_id.open_id, "feishu: user not in allowed_users");
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

    let mut kind = parse_message_kind(&msg)?;

    match &mut kind {
        MessageKind::Image { local_path, .. } => {
            if let Ok(img) = serde_json::from_str::<crate::types::ImageContent>(&msg.content)
                && !img.image_key.is_empty()
            {
                let save_dir = crate::media::media_save_dir();
                *local_path =
                    crate::media::download_image(http, &cfg.token_mgr, &img.image_key, &save_dir)
                        .await;
            }
        }
        MessageKind::File { local_path, .. } => {
            if let Ok(file) = serde_json::from_str::<crate::types::FileContent>(&msg.content)
                && !file.file_key.is_empty()
            {
                let save_dir = crate::media::media_save_dir();
                if let Some(path) = crate::media::download_file(
                    http,
                    &cfg.token_mgr,
                    &msg.message_id,
                    &file.file_key,
                    &file.file_name,
                    &save_dir,
                )
                .await
                {
                    *local_path = path;
                }
            }
        }
        _ => {}
    }

    let inbound = InboundMessage {
        peer,
        kind,
        received_at: SystemTime::now(),
    };

    tx.send(parse_inbound(inbound))
        .await
        .map_err(|_| Error::other("inbound channel closed"))?;
    Ok(())
}

fn parse_message_kind(msg: &crate::types::EventMessage) -> Result<MessageKind> {
    match msg.message_type.as_str() {
        "text" => {
            let content: crate::types::TextContent = serde_json::from_str(&msg.content)
                .map_err(|e| Error::Parse(format!("text content: {e}")))?;
            Ok(MessageKind::Text { text: content.text })
        }
        "image" => Ok(MessageKind::Image {
            local_path: None,
            caption: None,
        }),
        "file" => {
            let file: crate::types::FileContent = serde_json::from_str(&msg.content)
                .map_err(|e| Error::Parse(format!("file content: {e}")))?;
            Ok(MessageKind::File {
                local_path: std::path::PathBuf::new(),
                name: file.file_name,
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

async fn handle_card_action(
    cb: crate::types::EventCallback,
    cfg: &WsConfig,
    http: &reqwest::Client,
    tx: &mpsc::Sender<SourceMessage>,
) -> Result<()> {
    let event = match cb.event {
        Some(e) => e,
        None => return Ok(()),
    };

    let operator = match event.operator {
        Some(op) => op,
        None => return Ok(()),
    };

    if !cfg.allowed_users.is_empty() && !cfg.allowed_users.contains(&operator.open_id) {
        tracing::debug!(user_id = %operator.open_id, "feishu: card action user not in allowed_users");
        return Ok(());
    }

    let card_message_id = event.context.map(|c| c.open_message_id).unwrap_or_default();

    tracing::debug!(
        card_message_id = %card_message_id,
        "feishu: card action received"
    );

    let action_value = event
        .action
        .and_then(|a| a.value)
        .and_then(|v| v.get("action").and_then(|a| a.as_str()).map(String::from));

    let text = match action_value {
        Some(v) => v,
        None => {
            tracing::debug!("feishu: card action without action value");
            return Ok(());
        }
    };

    // Removed (not just looked up): the first click consumes the entry, so a
    // second click on the same card — the reported bug, no de-dup + no
    // visual change after clicking — finds nothing here and is dropped
    // instead of falling through to become a stray "y"/"n" chat message.
    let perm_card = if card_message_id.is_empty() {
        None
    } else {
        cfg.perm_cards.lock().await.remove(&card_message_id)
    };
    match perm_card {
        Some(entry) => {
            let status = match text.as_str() {
                "y" => t!("im.perm_resolved_once"),
                "s" => t!("im.perm_resolved_session"),
                _ => t!("im.perm_resolved_deny"),
            };
            let card_json = crate::types::build_permission_resolved_card(
                &entry.kind,
                &entry.what,
                entry.risk_icon,
                &entry.risk,
                &status,
            );
            if let Err(e) =
                crate::send::update_card(http, &cfg.token_mgr, &card_message_id, &card_json).await
            {
                tracing::warn!(error=%e, "feishu: failed to mark permission card resolved");
            }
        }
        None => {
            if !card_message_id.is_empty() {
                tracing::debug!(
                    message_id = %card_message_id,
                    "feishu: duplicate click on already-answered permission card; ignoring"
                );
                return Ok(());
            }
        }
    }

    let peer = PeerRef {
        user_id: operator.open_id,
        group_id: None,
        opaque: serde_json::json!({}),
    };

    let inbound = InboundMessage {
        peer,
        kind: MessageKind::Text { text },
        received_at: SystemTime::now(),
    };

    tx.send(parse_inbound(inbound))
        .await
        .map_err(|_| Error::other("inbound channel closed"))?;
    Ok(())
}
