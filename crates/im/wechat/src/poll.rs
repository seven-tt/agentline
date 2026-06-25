use crate::error::{Error, Result};
use crate::http::HttpClient;
use crate::types::{BaseInfo, GetUpdatesReq, GetUpdatesResp, WeixinMessage};
use agentline_im_core::parse_inbound;
use agentline_im_core::types::{InboundMessage, MessageKind, PeerRef, SourceMessage};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::task::JoinHandle;

const MIN_BACKOFF_SECS: u64 = 1;
const MAX_BACKOFF_SECS: u64 = 30;

/// The reserved keyword users send to top up an exhausted send budget. When the
/// queue is blocked, this is swallowed (token recorded, not forwarded to the
/// agent); otherwise it passes through as a normal message.
pub const CONTINUE_KEYWORD: &str = "继续";

pub type CursorCell = Arc<RwLock<String>>;

/// Shared in-memory map of `user_id → latest context_token`.
pub type ContextTokenCache = Arc<Mutex<HashMap<String, String>>>;

/// On-disk persistence delegate called by the poller on every advance.
pub trait CursorPersist: Send + Sync + 'static {
    fn save(&self, cursor: &str);
    /// Persist the most-recent `context_token` seen for a given user.
    /// Default implementation is a no-op (backward-compatible).
    fn save_context_token(&self, _user_id: &str, _token: &str) {}
}

pub struct NoopPersist;

impl CursorPersist for NoopPersist {
    fn save(&self, _cursor: &str) {}
}

/// Spawns the long-poll loop. Returns the JoinHandle plus an `mpsc::Receiver`
/// the bridge selects on.
pub fn spawn_poller(
    http: HttpClient,
    cursor: CursorCell,
    persist: Arc<dyn CursorPersist>,
    allowed_users: Arc<Vec<String>>,
    registry: crate::token::TokenRegistry,
    agent_done: Arc<tokio::sync::Mutex<std::collections::HashSet<String>>>,
    buffer: usize,
) -> (mpsc::Receiver<SourceMessage>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(buffer.max(1));
    let handle = tokio::spawn(async move {
        run_loop(
            http,
            cursor,
            persist,
            allowed_users,
            registry,
            agent_done,
            tx,
        )
        .await;
    });
    (rx, handle)
}

async fn run_loop(
    http: HttpClient,
    cursor: CursorCell,
    persist: Arc<dyn CursorPersist>,
    allowed_users: Arc<Vec<String>>,
    registry: crate::token::TokenRegistry,
    agent_done: Arc<tokio::sync::Mutex<std::collections::HashSet<String>>>,
    tx: mpsc::Sender<SourceMessage>,
) {
    // No identity endpoint exists in the iLink bot API; the bot_token itself
    // is the only handle we have, so log a masked suffix to tell accounts apart.
    let bot_token_tail = http
        .token()
        .await
        .map(|t| {
            let chars: Vec<char> = t.chars().collect();
            let start = chars.len().saturating_sub(6);
            chars[start..].iter().collect::<String>()
        })
        .unwrap_or_default();
    tracing::info!(bot_token_tail, "wechat: polling started");

    let mut backoff_secs = MIN_BACKOFF_SECS;
    loop {
        let buf = cursor.read().await.clone();
        let req = GetUpdatesReq {
            get_updates_buf: buf,
            base_info: BaseInfo::current(),
        };
        let result: Result<GetUpdatesResp> = http
            .post_json_with_timeout("/ilink/bot/getupdates", &req, Some(Duration::from_secs(45)))
            .await;
        match result {
            Ok(resp) => {
                if resp.ret != 0 {
                    tracing::warn!(ret=resp.ret, msg=?resp.errmsg, "getupdates non-zero ret");
                    sleep_backoff(&mut backoff_secs).await;
                    continue;
                }
                backoff_secs = MIN_BACKOFF_SECS;
                if let Some(new_cursor) = resp.get_updates_buf {
                    {
                        let mut c = cursor.write().await;
                        if *c != new_cursor {
                            *c = new_cursor.clone();
                            persist.save(&new_cursor);
                        }
                    }
                }
                for raw in resp.msgs {
                    // Extract media refs before convert consumes raw.
                    let image_item = raw.item_list.first().and_then(|i| i.image_item.clone());
                    let file_item = raw.item_list.first().and_then(|i| i.file_item.clone());
                    let video_item = raw.item_list.first().and_then(|i| i.video_item.clone());

                    if let Some(mut msg) = convert(&raw, &allowed_users) {
                        // Download image / voice / file / video media so the agent can see them.
                        match &mut msg.kind {
                            MessageKind::Image { local_path, .. } => {
                                if let Some(ref img) = image_item {
                                    let save_dir = media_save_dir();
                                    if let Some(path) =
                                        crate::media::download_image(&http, img, &save_dir).await
                                    {
                                        *local_path = Some(path);
                                    }
                                }
                            }
                            // Voice messages carry iLink's server-side transcription
                            // (voice_item.text), already mapped to Voice.transcript in
                            // convert(). The raw media is SILK we can't transcode, so we
                            // don't download it.
                            MessageKind::File { local_path, .. } => {
                                if let Some(ref file) = file_item {
                                    let save_dir = media_save_dir();
                                    if let Some(path) =
                                        crate::media::download_file(&http, file, &save_dir).await
                                    {
                                        *local_path = path;
                                    }
                                }
                            }
                            MessageKind::Video { local_path, .. } => {
                                if let Some(ref video) = video_item {
                                    let save_dir = media_save_dir();
                                    if let Some(path) =
                                        crate::media::download_video(&http, video, &save_dir).await
                                    {
                                        *local_path = Some(path);
                                    }
                                }
                            }
                            _ => {}
                        }

                        // Is this a bare "继续" sent purely to top up an exhausted
                        // send budget? Check *before* recording the new token,
                        // since recording resets the exhausted state.
                        let is_continue = matches!(
                            &msg.kind,
                            MessageKind::Text { text } if text.trim() == CONTINUE_KEYWORD
                        );
                        let was_blocked = registry.is_exhausted(&msg.peer.user_id);

                        // Top up this user's send budget with the fresh token,
                        // and persist it as their latest for restart recovery.
                        if let Some(token) = msg.peer.opaque["context_token"].as_str() {
                            let token = token.to_string();
                            registry.record(&msg.peer.user_id, &token);
                            persist.save_context_token(&msg.peer.user_id, &token);
                        }

                        // A "继续" whose only job was to unblock the queue must NOT
                        // be forwarded to the agent as a new prompt (it would spawn
                        // another turn and pile more output onto the backlog).
                        if is_continue && was_blocked {
                            tracing::info!(user_id = %msg.peer.user_id, "继续 swallowed as token top-up (not forwarded to agent)");
                            continue;
                        }
                        agent_done.lock().await.remove(&msg.peer.user_id);
                        if tx.send(parse_inbound(msg)).await.is_err() {
                            return; // bridge dropped, stop polling
                        }
                    }
                }
            }
            Err(Error::Http(s)) if s.contains("401") || s.contains("403") => {
                tracing::error!("auth rejected ({s}); poller exiting");
                return;
            }
            Err(e) => {
                tracing::warn!(error=%e, "getupdates failed; backing off");
                sleep_backoff(&mut backoff_secs).await;
            }
        }
    }
}

fn media_save_dir() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join(".agentline")
        .join("media")
        .join("inbound")
}

async fn sleep_backoff(backoff_secs: &mut u64) {
    tokio::time::sleep(Duration::from_secs(*backoff_secs)).await;
    *backoff_secs = (*backoff_secs * 2).min(MAX_BACKOFF_SECS);
}

/// Translate one raw iLink message into the core's `InboundMessage`.
/// Returns `None` for messages we should not route to the bridge:
/// - bot's own outbound (`message_type == 2`)
/// - non-FINISH frames
/// - sender not on the allow-list (when the list is non-empty)
fn convert(raw: &WeixinMessage, allowed: &[String]) -> Option<InboundMessage> {
    if raw.message_type != 1 {
        return None;
    }
    if raw.message_state != 2 {
        // Skip partial frames; only route complete messages.
        return None;
    }
    tracing::info!(user_id = %raw.from_user_id, "wechat: message from user");
    if !allowed.is_empty() && !allowed.iter().any(|u| u == &raw.from_user_id) {
        tracing::debug!(user_id = %raw.from_user_id, "wechat: user not in allow-list, dropping");
        return None;
    }

    let kind = item_to_message_kind(raw)?;

    let opaque = serde_json::json!({
        "context_token": raw.context_token,
    });
    let peer = PeerRef {
        user_id: raw.from_user_id.clone(),
        group_id: raw.group_id.clone().filter(|g| !g.is_empty()),
        opaque,
    };
    Some(InboundMessage {
        peer,
        kind,
        received_at: SystemTime::now(),
    })
}

fn item_to_message_kind(raw: &WeixinMessage) -> Option<MessageKind> {
    let item = raw.item_list.first()?;
    match item.item_type {
        1 => item.text_item.as_ref().map(|t| MessageKind::Text {
            text: t.text.clone(),
        }),
        3 => Some(MessageKind::Voice {
            transcript: item.voice_item.as_ref().and_then(|v| v.text.clone()),
            local_path: None,
        }),
        2 => Some(MessageKind::Image {
            local_path: None,
            caption: None,
        }),
        4 => {
            let file = item.file_item.as_ref()?;
            Some(MessageKind::File {
                local_path: std::path::PathBuf::new(),
                name: file.file_name.clone().unwrap_or_default(),
            })
        }
        5 => Some(MessageKind::Video {
            local_path: None,
            caption: None,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Item, TextItem, VoiceItem};

    fn raw_text(from: &str, text: &str) -> WeixinMessage {
        WeixinMessage {
            from_user_id: from.into(),
            to_user_id: "bot@im.bot".into(),
            message_type: 1,
            message_state: 2,
            context_token: "ctx".into(),
            client_id: None,
            group_id: None,
            item_list: vec![Item {
                item_type: 1,
                text_item: Some(TextItem { text: text.into() }),
                image_item: None,
                voice_item: None,
                file_item: None,
                video_item: None,
                tool_call_start_item: None,
                tool_call_result_item: None,
                thinking_item: None,
                stream_signal_item: None,
            }],
            extra: Default::default(),
        }
    }

    #[test]
    fn passes_text_message() {
        let raw = raw_text("u@im.wechat", "hi");
        let im = convert(&raw, &[]).unwrap();
        match im.kind {
            MessageKind::Text { text } => assert_eq!(text, "hi"),
            _ => panic!("wrong kind"),
        }
        assert_eq!(im.peer.user_id, "u@im.wechat");
        assert_eq!(im.peer.opaque["context_token"], "ctx");
    }

    #[test]
    fn drops_outbound_echo() {
        let mut raw = raw_text("u@im.wechat", "hi");
        raw.message_type = 2;
        assert!(convert(&raw, &[]).is_none());
    }

    #[test]
    fn enforces_allow_list() {
        let raw = raw_text("u@im.wechat", "hi");
        assert!(convert(&raw, &["other@im.wechat".to_string()]).is_none());
        assert!(convert(&raw, &["u@im.wechat".to_string()]).is_some());
    }

    #[test]
    fn voice_with_transcript_becomes_voice_kind() {
        let raw = WeixinMessage {
            from_user_id: "u".into(),
            to_user_id: "b".into(),
            message_type: 1,
            message_state: 2,
            context_token: "c".into(),
            client_id: None,
            group_id: None,
            item_list: vec![Item {
                item_type: 3,
                text_item: None,
                image_item: None,
                voice_item: Some(VoiceItem {
                    text: Some("说了句话".into()),
                    media: None,
                    extra: Default::default(),
                }),
                file_item: None,
                video_item: None,
                tool_call_start_item: None,
                tool_call_result_item: None,
                thinking_item: None,
                stream_signal_item: None,
            }],
            extra: Default::default(),
        };
        let im = convert(&raw, &[]).unwrap();
        match im.kind {
            MessageKind::Voice { transcript, .. } => {
                assert_eq!(transcript.as_deref(), Some("说了句话"));
            }
            _ => panic!("wrong kind"),
        }
    }
}
