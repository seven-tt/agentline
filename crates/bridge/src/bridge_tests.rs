use super::*;
use crate::agent::AgentBackend;
use crate::event::{OutboundEvent, TextFormat};
use crate::permission::PermissionDanger;
use crate::router::SourceRouter;
use crate::session::SessionKey;
use crate::source::{ImAdapter, InboundHandler, InputSource, InputSourceKind};
use crate::types::{
    AgentUpdate, Command, CwdResult, InboundMessage, InboundPayload, MessageKind, RoutedMessage,
    ToolKind, UserContent, YoloResult,
};
use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use std::sync::Mutex as StdMutex;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::mpsc;

type SentLog = Arc<StdMutex<Vec<(String, String)>>>;

struct MockSource {
    sender: TokioMutex<Option<mpsc::Sender<InboundMessage>>>,
    receiver: TokioMutex<Option<mpsc::Receiver<InboundMessage>>>,
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

    async fn feed(&self, msg: InboundMessage) {
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
    async fn start(&self) -> Result<mpsc::Receiver<InboundMessage>> {
        self.receiver
            .lock()
            .await
            .take()
            .ok_or_else(|| crate::error::Error::other("already started"))
    }
    async fn send_event(&self, to: &PeerRef, event: &OutboundEvent) -> Result<()> {
        use crate::event::ToolEvent;
        match event {
            OutboundEvent::Text { content, .. } | OutboundEvent::StreamChunk { text: content } => {
                self.send_text(to, content).await
            }
            OutboundEvent::Tool(ToolEvent::Start { kind, label, .. }) => {
                self.send_text(to, &crate::format::tool_label(*kind, label))
                    .await
            }
            OutboundEvent::Tool(ToolEvent::End { ok, summary, .. }) => {
                let mark = if *ok { "✅" } else { "❌" };
                let body = summary
                    .as_deref()
                    .unwrap_or(if *ok { "done" } else { "failed" });
                self.send_text(to, &format!("{mark} {body}")).await
            }
            OutboundEvent::PermissionRequest {
                what, tool_kind, ..
            } => {
                self.send_text(to, &format!("⚠️ {} 需要授权: {what}", tool_kind.name()))
                    .await
            }
            OutboundEvent::Error(msg) => self.send_text(to, &format!("❌ {msg}")).await,
            _ => Ok(()),
        }
    }
    fn parse_message(&self, msg: &InboundMessage) -> InboundPayload {
        match &msg.kind {
            MessageKind::Text { text } => {
                let trimmed = text.trim();
                if trimmed.starts_with('/') {
                    let cmd = trimmed.split_whitespace().next().unwrap_or("");
                    match cmd {
                        "/help" | "/?" => InboundPayload::Command(Command::Help),
                        "/yolo" => InboundPayload::Command(Command::Yolo),
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
                }
            }
            _ => InboundPayload::Content(UserContent {
                text: None,
                attachments: Vec::new(),
            }),
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
    sessions: StdMutex<Vec<SessionId>>,
    cancelled: Arc<StdMutex<bool>>,
    permission_answers: Arc<StdMutex<Vec<(String, bool)>>>,
}

#[async_trait]
impl AgentBackend for MockAgent {
    async fn new_session(&self, _cwd: &std::path::Path) -> Result<SessionId> {
        let sid = SessionId::new(format!("sess-{}", self.sessions.lock().unwrap().len()));
        self.sessions.lock().unwrap().push(sid.clone());
        Ok(sid)
    }

    async fn prompt<'a>(
        &'a self,
        _sid: &'a SessionId,
        text: &'a str,
    ) -> Result<BoxStream<'a, AgentUpdate>> {
        let mut updates = vec![
            AgentUpdate::ToolCallStart {
                id: "t1".into(),
                kind: ToolKind::Shell,
                label: format!("echo {text}"),
            },
            AgentUpdate::ToolCallEnd {
                id: "t1".into(),
                ok: true,
                summary: None,
            },
            AgentUpdate::AssistantText {
                delta: format!("回声: {text}"),
                is_final: true,
            },
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

    async fn cancel(&self, _sid: &SessionId) -> Result<()> {
        *self.cancelled.lock().unwrap() = true;
        Ok(())
    }

    async fn answer_permission(
        &self,
        _sid: &SessionId,
        request_id: &str,
        allow: bool,
    ) -> Result<()> {
        self.permission_answers
            .lock()
            .unwrap()
            .push((request_id.to_string(), allow));
        Ok(())
    }

    async fn close_session(&self, _sid: SessionId) -> Result<()> {
        Ok(())
    }
}

fn make_peer() -> PeerRef {
    PeerRef {
        user_id: "user-A".into(),
        group_id: None,
        opaque: serde_json::Value::Null,
    }
}

fn make_text_msg(t: &str) -> InboundMessage {
    InboundMessage {
        peer: make_peer(),
        kind: MessageKind::Text { text: t.into() },
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

struct TestInboundHandler;

#[async_trait]
impl InboundHandler for TestInboundHandler {
    async fn handle(&self, bridge: &Bridge, routed: RoutedMessage) -> Result<()> {
        let source_id = routed.source_id;
        let peer = routed.peer;
        let key = SessionKey::new(&source_id, &peer);
        let send_text = |text: String| {
            let sid = source_id.clone();
            let p = peer.clone();
            let b = bridge.clone();
            async move {
                b.send_event(
                    &sid,
                    &p,
                    &OutboundEvent::Text {
                        content: text,
                        format: TextFormat::Plain,
                    },
                )
                .await
            }
        };
        match routed.payload {
            InboundPayload::Command(cmd) => match cmd {
                Command::Help => {
                    send_text("Agentline help".into()).await?;
                }
                Command::Cd(path) => {
                    let msg = match bridge.set_cwd(path.clone(), &key).await {
                        CwdResult::Changed | CwdResult::SessionClosed { .. } => {
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
                    let msg = match bridge.set_yolo(true, &key).await {
                        YoloResult::Applied { tag } => format!("yolo on {tag}"),
                        YoloResult::Pending => "yolo pending".into(),
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
                let text = content.to_prompt_text();
                bridge.send_prompt(key, peer, text, source_id).await?;
            }
        }
        Ok(())
    }
}

fn make_bridge(source: Arc<MockSource>, agent: Arc<MockAgent>) -> (Bridge, JoinHandle<()>) {
    let mut router = SourceRouter::new();
    router.register_im(source);
    Bridge::from_router(router, agent, cfg_with_tmp(), Arc::new(TestInboundHandler))
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
async fn unknown_slash_shows_help() {
    let (source, sent) = MockSource::new();
    let agent = Arc::new(MockAgent::default());
    let (_, actor_handle) = make_bridge(source.clone(), agent);
    run_with_msgs(&source, actor_handle, &["/compact"]).await;
    let sent = sent.lock().unwrap();
    assert!(sent.iter().any(|(_, t)| t.contains("Agentline")));
}
