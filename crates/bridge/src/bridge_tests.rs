use super::*;
use crate::agent::{AgentBackend, AgentFactory};
use crate::event::AgentEvent;
use crate::permission::PermissionDanger;
use crate::router::SourceRouter;
use crate::source::{ImAdapter, InboundHandler, InputSource, InputSourceKind};
use crate::types::{
    AgentSessionId, AgentUpdate, Command, ContentBlock, CwdResult, InboundPayload, ModeResult,
    PeerRef, RoutedMessage, SessionId, SourceMessage, ToolKind, UserContent,
};
use agent_client_protocol::{
    ContentChunk, SessionUpdate, TextContent, ToolCall, ToolCallStatus, ToolCallUpdate,
    ToolCallUpdateFields,
};
use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use std::collections::HashMap;
use std::sync::Mutex as StdMutex;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::mpsc;

type SentLog = Arc<StdMutex<Vec<(String, String)>>>;

struct MockSource {
    sender: TokioMutex<Option<mpsc::Sender<SourceMessage>>>,
    receiver: TokioMutex<Option<mpsc::Receiver<SourceMessage>>>,
    sent: SentLog,
}

impl MockSource {
    fn new() -> (Arc<Self>, SentLog) {
        let (tx, rx) = mpsc::channel(16);
        let sent: SentLog = Arc::new(StdMutex::new(Vec::new()));
        let s = Arc::new(Self {
            sender: TokioMutex::new(Some(tx)),
            receiver: TokioMutex::new(Some(rx)),
            sent: sent.clone(),
        });
        (s, sent)
    }

    async fn feed(&self, msg: SourceMessage) {
        if let Some(tx) = self.sender.lock().await.as_ref() {
            tx.send(msg).await.unwrap();
        }
    }

    async fn close(&self) {
        self.sender.lock().await.take();
    }
}

#[async_trait]
impl InputSource for MockSource {
    fn id(&self) -> &str {
        "mock"
    }
    fn kind(&self) -> InputSourceKind {
        InputSourceKind::Im
    }
    async fn start(&self) -> Result<mpsc::Receiver<SourceMessage>> {
        self.receiver
            .lock()
            .await
            .take()
            .ok_or_else(|| crate::error::Error::other("already started"))
    }
    async fn send_update(&self, to: &PeerRef, event: &AgentEvent) -> Result<()> {
        match &event.update {
            AgentUpdate::Session(su) => match su {
                SessionUpdate::AgentMessageChunk(chunk) => {
                    if let agent_client_protocol::ContentBlock::Text(t) = &chunk.content
                        && !t.text.is_empty()
                    {
                        self.send_text(to, &t.text).await?;
                    }
                    Ok(())
                }
                SessionUpdate::ToolCall(tc) => {
                    let kind = crate::acp_mapping::map_tool_kind(tc.kind);
                    self.send_text(to, &crate::format::tool_label(kind, &tc.title))
                        .await
                }
                SessionUpdate::ToolCallUpdate(tcu) => {
                    if let Some((ok, summary)) = crate::acp_mapping::tool_call_update_status(tcu) {
                        let mark = if ok { "✅" } else { "❌" };
                        let body = summary
                            .as_deref()
                            .unwrap_or(if ok { "done" } else { "failed" });
                        self.send_text(to, &format!("{mark} {body}")).await?;
                    }
                    Ok(())
                }
                _ => Ok(()),
            },
            AgentUpdate::PermissionRequest {
                what, tool_kind, ..
            } => {
                self.send_text(to, &format!("⚠️ {} 需要授权: {what}", tool_kind.name()))
                    .await
            }
            AgentUpdate::Error(msg) => self.send_text(to, &format!("❌ {msg}")).await,
            _ => Ok(()),
        }
    }
    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl ImAdapter for MockSource {
    async fn send_text(&self, to: &PeerRef, text: &str) -> Result<()> {
        self.sent
            .lock()
            .unwrap()
            .push((to.user_id.clone(), text.to_string()));
        Ok(())
    }
}

#[derive(Default)]
struct MockAgent {
    sessions: StdMutex<Vec<AgentSessionId>>,
    cancelled: Arc<StdMutex<bool>>,
    permission_answers: Arc<StdMutex<Vec<(String, bool)>>>,
}

#[async_trait]
impl AgentBackend for MockAgent {
    async fn new_session(&self, _cwd: &std::path::Path) -> Result<AgentSessionId> {
        let sid = AgentSessionId::new(format!("sess-{}", self.sessions.lock().unwrap().len()));
        self.sessions.lock().unwrap().push(sid.clone());
        Ok(sid)
    }

    async fn prompt<'a>(
        &'a self,
        _sid: &'a AgentSessionId,
        content: &'a [ContentBlock],
    ) -> Result<BoxStream<'a, AgentUpdate>> {
        let text = crate::types::content_to_text(content);
        let mut updates = vec![
            AgentUpdate::Session(SessionUpdate::ToolCall(
                ToolCall::new("t1", format!("echo {text}"))
                    .kind(agent_client_protocol::ToolKind::Execute)
                    .status(ToolCallStatus::InProgress),
            )),
            AgentUpdate::Session(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                "t1",
                ToolCallUpdateFields::new().status(ToolCallStatus::Completed),
            ))),
            AgentUpdate::Session(SessionUpdate::AgentMessageChunk(ContentChunk::new(
                agent_client_protocol::ContentBlock::Text(TextContent::new(format!(
                    "回声: {text}"
                ))),
            ))),
            AgentUpdate::Done,
        ];
        if text.contains("@perm") {
            updates.insert(
                0,
                AgentUpdate::PermissionRequest {
                    id: "p1".into(),
                    what: "rm /tmp/x".into(),
                    danger: PermissionDanger::High,
                    tool_kind: ToolKind::Shell,
                },
            );
        }
        Ok(stream::iter(updates).boxed())
    }

    async fn cancel(&self, _sid: &AgentSessionId) -> Result<()> {
        *self.cancelled.lock().unwrap() = true;
        Ok(())
    }

    async fn respond_permission(
        &self,
        _sid: &AgentSessionId,
        request_id: &str,
        allow: bool,
    ) -> Result<()> {
        self.permission_answers
            .lock()
            .unwrap()
            .push((request_id.to_string(), allow));
        Ok(())
    }

    async fn close_session(&self, _sid: AgentSessionId) -> Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct MockFactory;

#[async_trait]
impl AgentFactory for MockFactory {
    fn available(&self) -> Vec<String> {
        vec!["mock-agent".to_string()]
    }
    async fn build(&self, _name: &str) -> Result<Arc<dyn AgentBackend>> {
        Ok(Arc::new(MockAgent::default()))
    }
}

fn make_peer() -> PeerRef {
    PeerRef {
        user_id: "user-A".into(),
        group_id: None,
        opaque: serde_json::Value::Null,
    }
}

fn make_text_msg(t: &str) -> SourceMessage {
    let trimmed = t.trim();
    let payload = if trimmed.starts_with('/') {
        let cmd = trimmed.split_whitespace().next().unwrap_or("");
        match cmd {
            "/help" | "/?" => InboundPayload::Command(Command::Help),
            "/yolo" => InboundPayload::Command(Command::Yolo),
            "/agent" => {
                let arg = trimmed.strip_prefix("/agent").unwrap().trim();
                if arg.is_empty() {
                    InboundPayload::Command(Command::Agent(None))
                } else {
                    InboundPayload::Command(Command::Agent(Some(arg.to_string())))
                }
            }
            "/cd" => {
                let arg = trimmed.strip_prefix("/cd").unwrap().trim();
                if arg.is_empty() {
                    InboundPayload::Command(Command::CdInteractive)
                } else {
                    InboundPayload::Command(Command::Cd(arg.into()))
                }
            }
            _ => InboundPayload::Command(Command::Help),
        }
    } else {
        InboundPayload::Content(UserContent::text(trimmed))
    };
    SourceMessage {
        peer: make_peer(),
        payload,
        received_at: std::time::SystemTime::now(),
    }
}

fn cfg_with_tmp() -> BridgeConfig {
    BridgeConfig {
        default_cwd: std::env::temp_dir(),
        typing_interval: Duration::from_secs(60),
        ..Default::default()
    }
}

#[derive(Default)]
struct TestInboundHandler {
    sessions: TokioMutex<HashMap<String, SessionId>>,
}

fn peer_key(source_id: &str, peer: &PeerRef) -> String {
    format!("{source_id}|{}|{:?}", peer.user_id, peer.group_id)
}

#[async_trait]
impl InboundHandler for TestInboundHandler {
    async fn handle(&self, bridge: &Bridge, routed: RoutedMessage) -> Result<()> {
        let source_id = routed.source_id;
        let peer = routed.peer;
        let pkey = peer_key(&source_id, &peer);
        let cached_sid = self.sessions.lock().await.get(&pkey).cloned();
        let send_text = |text: String| {
            let sid = source_id.clone();
            let p = peer.clone();
            let b = bridge.clone();
            async move { b.reply(&sid, &p, &text, false).await }
        };
        match routed.payload {
            InboundPayload::Command(cmd) => match cmd {
                Command::Help => {
                    send_text("Agentline help".into()).await?;
                }
                Command::Cd(path) => {
                    let msg = match bridge.set_cwd(path.clone(), cached_sid.as_ref()).await {
                        CwdResult::Changed => format!("cd {}", path.display()),
                        CwdResult::SessionClosed { .. } => {
                            self.sessions.lock().await.remove(&pkey);
                            format!("cd {}", path.display())
                        }
                        CwdResult::NotFound => {
                            format!("路径不存在: {}", path.display())
                        }
                        CwdResult::NotADir => {
                            format!("不是目录: {}", path.display())
                        }
                    };
                    send_text(msg).await?;
                }
                Command::Yolo => {
                    let msg = match bridge.set_mode("yolo", cached_sid.as_ref()).await {
                        ModeResult::Applied { tag } => format!("yolo on {tag}"),
                        ModeResult::Pending => "yolo pending".into(),
                        ModeResult::NoSession => "no session".into(),
                    };
                    send_text(msg).await?;
                }
                Command::Agent(Some(name)) => {
                    let msg = match bridge.set_provider(&name).await {
                        Ok(()) => format!("switched to {name}"),
                        Err(e) => format!("switch failed: {e}"),
                    };
                    send_text(msg).await?;
                }
                _ => {
                    send_text("Agentline help".into()).await?;
                }
            },
            InboundPayload::Content(content) => {
                if content.is_empty() {
                    return Ok(());
                }
                let sid = match cached_sid {
                    Some(sid) => sid,
                    None => {
                        let sid = bridge.new_session(source_id.clone(), peer.clone()).await?;
                        self.sessions.lock().await.insert(pkey.clone(), sid.clone());
                        sid
                    }
                };
                bridge.prompt(sid, content.to_content_blocks()).await?;
            }
        }
        Ok(())
    }

    async fn invalidate_all_sessions(&self) {
        self.sessions.lock().await.clear();
    }
}

fn make_bridge(source: Arc<MockSource>, agent: Arc<MockAgent>) -> (Bridge, JoinHandle<()>) {
    let mut router = SourceRouter::new();
    router.register_im(source);
    Bridge::from_router(
        router,
        agent,
        cfg_with_tmp(),
        Arc::new(TestInboundHandler::default()),
    )
}

async fn run_with_msgs(source: &Arc<MockSource>, actor_handle: JoinHandle<()>, msgs: &[&str]) {
    let src = source.clone();
    for m in msgs {
        src.feed(make_text_msg(m)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    src.close().await;
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn happy_path_echo() {
    let (source, sent) = MockSource::new();
    let agent = Arc::new(MockAgent::default());
    let (_, actor_handle) = make_bridge(source.clone(), agent);
    run_with_msgs(&source, actor_handle, &["hello"]).await;
    let sent = sent.lock().unwrap();
    let texts: Vec<_> = sent.iter().map(|(_, t)| t.as_str()).collect();
    assert!(texts.iter().any(|t| t.contains("echo hello")));
    assert!(texts.iter().any(|t| t.contains("回声: hello")));
}

#[tokio::test]
async fn cd_invalid_path_reports() {
    let (source, sent) = MockSource::new();
    let agent = Arc::new(MockAgent::default());
    let (_, actor_handle) = make_bridge(source.clone(), agent);
    run_with_msgs(&source, actor_handle, &["/cd /nope/nada/123abc"]).await;
    let sent = sent.lock().unwrap();
    assert!(sent.iter().any(|(_, t)| t.contains("路径不存在")));
}

#[tokio::test]
async fn help_emits_help_text() {
    let (source, sent) = MockSource::new();
    let agent = Arc::new(MockAgent::default());
    let (_, actor_handle) = make_bridge(source.clone(), agent);
    run_with_msgs(&source, actor_handle, &["/help"]).await;
    let sent = sent.lock().unwrap();
    assert!(sent.iter().any(|(_, t)| t.contains("Agentline")));
}

#[tokio::test]
async fn yolo_auto_approves_permission() {
    let (source, sent) = MockSource::new();
    let agent = Arc::new(MockAgent::default());
    let answers = agent.permission_answers.clone();
    let (_, actor_handle) = make_bridge(source.clone(), agent);
    run_with_msgs(&source, actor_handle, &["/yolo", "@perm please"]).await;
    let answers = answers.lock().unwrap();
    assert!(answers.iter().any(|(id, allow)| id == "p1" && *allow));
    let sent = sent.lock().unwrap();
    assert!(!sent.iter().any(|(_, t)| t.contains("需要授权")));
}

#[tokio::test]
async fn agent_switch_invalidates_handler_cache_so_next_message_still_works() {
    // The real-world bug: `/agent` drains every bridge-side session, but the
    // IM handler's own peer→SessionId cache doesn't know that. Without
    // `cmd_set_provider` calling `handler.invalidate_all_sessions()`, the
    // next message from the same peer reuses the now-dead cached id and goes
    // nowhere. Bridge::with_agent_factory + a real `/agent <name>` round trip
    // exercises the exact path cmd_set_provider takes (factory lookup, drain,
    // invalidate) rather than testing the drain in isolation.
    let (source, sent) = MockSource::new();
    let agent = Arc::new(MockAgent::default());
    let (bridge, actor_handle) = make_bridge(source.clone(), agent);
    bridge.clone().with_agent_factory(Arc::new(MockFactory));

    run_with_msgs(
        &source,
        actor_handle,
        &["hello", "/agent mock-agent", "hello again"],
    )
    .await;

    let sent = sent.lock().unwrap();
    let texts: Vec<_> = sent.iter().map(|(_, t)| t.as_str()).collect();
    assert!(texts.iter().any(|t| t.contains("switched to mock-agent")));
    assert!(
        texts.iter().any(|t| t.contains("回声: hello again")),
        "message sent right after /agent switch got no reply: {texts:?}"
    );
}

#[tokio::test]
async fn prompt_after_session_closed_errors_instead_of_vanishing() {
    // A caller (e.g. the IM handler) can hold a SessionId for a session that
    // no longer exists bridge-side — `/agent` draining every session is the
    // real-world trigger. `prompt` must reject it so the caller can recover
    // (forget the stale id, create a fresh session) instead of the message
    // being queued and silently dropped with no reply ever coming back.
    let (source, _sent) = MockSource::new();
    let agent = Arc::new(MockAgent::default());
    let (bridge, actor_handle) = make_bridge(source.clone(), agent);

    let sid = bridge
        .new_session("mock".into(), make_peer())
        .await
        .unwrap();
    bridge.close_session(&sid).await.unwrap();

    let result = bridge.prompt(sid, crate::types::text_prompt("hi")).await;
    assert!(
        matches!(result, Err(crate::error::Error::SessionNotFound(_))),
        "expected SessionNotFound, got {result:?}"
    );

    source.close().await;
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn unknown_slash_shows_help() {
    let (source, sent) = MockSource::new();
    let agent = Arc::new(MockAgent::default());
    let (_, actor_handle) = make_bridge(source.clone(), agent);
    run_with_msgs(&source, actor_handle, &["/compact"]).await;
    let sent = sent.lock().unwrap();
    assert!(sent.iter().any(|(_, t)| t.contains("Agentline")));
}
