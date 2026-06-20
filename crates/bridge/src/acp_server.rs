//! ACP server: makes agentline a protocol-compliant ACP agent.
//!
//! Three pieces:
//! - [`AcpSource`]: an [`InputSource`] that bridges ACP client traffic into
//!   the SourceRouter.
//! - [`AgentlineAgent`]: implements the ACP [`Agent`] trait, translating each
//!   ACP request into Bridge operations.
//!
//! The ACP connection is `!Send` (JSON-RPC layer uses `Rc`/`RefCell`), so all
//! outbound ACP calls (session_notification, request_permission, etc.) run on
//! a `spawn_local` task driven by an mpsc channel. `send_update` enqueues
//! commands and returns immediately; a local task processes them.

use agent_client_protocol::{self as acp, Client as _};
use async_trait::async_trait;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::bridge::Bridge;
use crate::error::Result;
use crate::event::AgentEvent;
use crate::source::{InputSource, InputSourceKind};
use crate::types::{AgentUpdate, PeerRef, SessionId, SourceMessage};

const DEFAULT_SOURCE_ID: &str = "acp";

// ── Outbound command channel (Send → !Send bridge) ───────────────────────

pub enum OutCmd {
    SessionNotification {
        sid: SessionId,
        update: acp::SessionUpdate,
    },
    PermissionRequest {
        sid: SessionId,
        id: String,
        what: String,
    },
    ElicitInput {
        sid: SessionId,
        prompt: String,
        schema: Option<acp::ElicitationSchema>,
    },
    Done {
        sid: SessionId,
    },
    Error {
        sid: SessionId,
    },
}

// ── AcpSource ────────────────────────────────────────────────────────────

pub struct AcpSource {
    source_id: String,
    inbound_rx: Mutex<Option<mpsc::Receiver<SourceMessage>>>,
    out_tx: mpsc::UnboundedSender<OutCmd>,
    completions: Arc<Mutex<HashMap<SessionId, oneshot::Sender<acp::StopReason>>>>,
}

impl AcpSource {
    /// Create an AcpSource with the default source id ("acp").
    pub fn new() -> (Arc<Self>, mpsc::UnboundedReceiver<OutCmd>) {
        Self::with_id(DEFAULT_SOURCE_ID.to_string())
    }

    /// Create an AcpSource with a custom source id (for multi-connection
    /// transports where each connection gets a unique id like "acp:0").
    pub fn with_id(source_id: String) -> (Arc<Self>, mpsc::UnboundedReceiver<OutCmd>) {
        let (_inbound_tx, inbound_rx) = mpsc::channel::<SourceMessage>(64);
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let source = Arc::new(Self {
            source_id,
            inbound_rx: Mutex::new(Some(inbound_rx)),
            out_tx,
            completions: Arc::new(Mutex::new(HashMap::new())),
        });
        (source, out_rx)
    }

    pub async fn register_completion(&self, sid: SessionId) -> oneshot::Receiver<acp::StopReason> {
        let (tx, rx) = oneshot::channel();
        self.completions.lock().await.insert(sid, tx);
        rx
    }
}

#[async_trait]
impl InputSource for AcpSource {
    fn id(&self) -> &str {
        &self.source_id
    }

    fn kind(&self) -> InputSourceKind {
        InputSourceKind::Remote
    }

    async fn start(&self) -> Result<mpsc::Receiver<SourceMessage>> {
        self.inbound_rx
            .lock()
            .await
            .take()
            .ok_or_else(|| crate::error::Error::other("AcpSource already started"))
    }

    async fn send_update(&self, _to: &PeerRef, event: &AgentEvent) -> Result<()> {
        let sid = event.session_id.clone();
        let cmd = match &event.update {
            AgentUpdate::Session(su) => OutCmd::SessionNotification {
                sid,
                update: su.clone(),
            },
            AgentUpdate::PermissionRequest { id, what, .. } => OutCmd::PermissionRequest {
                sid,
                id: id.clone(),
                what: what.clone(),
            },
            AgentUpdate::ElicitInput { prompt, schema, .. } => OutCmd::ElicitInput {
                sid,
                prompt: prompt.clone(),
                schema: schema.clone(),
            },
            AgentUpdate::Done => OutCmd::Done { sid },
            AgentUpdate::Error(_) => OutCmd::Error { sid },
        };
        let _ = self.out_tx.send(cmd);
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

// ── Local-task outbound processor ────────────────────────────────────────

async fn process_outbound(
    conn: acp::AgentSideConnection,
    bridge: Bridge,
    completions: Arc<Mutex<HashMap<SessionId, oneshot::Sender<acp::StopReason>>>>,
    mut out_rx: mpsc::UnboundedReceiver<OutCmd>,
) {
    while let Some(cmd) = out_rx.recv().await {
        match cmd {
            OutCmd::SessionNotification { sid, update } => {
                let notif = acp::SessionNotification::new(sid.0.clone(), update);
                let _ = conn.session_notification(notif).await;
            }
            OutCmd::PermissionRequest { sid, id, what } => {
                let options = vec![
                    acp::PermissionOption::new(
                        "allow",
                        "Allow",
                        acp::PermissionOptionKind::AllowOnce,
                    ),
                    acp::PermissionOption::new(
                        "deny",
                        "Deny",
                        acp::PermissionOptionKind::RejectOnce,
                    ),
                ];
                let tool_call =
                    acp::ToolCallUpdate::new(id, acp::ToolCallUpdateFields::new().title(what));
                let req = acp::RequestPermissionRequest::new(sid.0.clone(), tool_call, options);
                let resp = conn.request_permission(req).await;
                let allow = match resp {
                    Ok(r) => matches!(
                        r.outcome,
                        acp::RequestPermissionOutcome::Selected(ref s)
                            if s.option_id.0.as_ref() == "allow"
                    ),
                    Err(_) => false,
                };
                let response = if allow {
                    crate::permission::PermissionResponse::Once
                } else {
                    crate::permission::PermissionResponse::Deny
                };
                bridge.respond_permission(response, &sid).await;
            }
            OutCmd::ElicitInput {
                sid,
                prompt,
                schema,
            } => {
                let form_schema = schema.clone().unwrap_or_else(|| {
                    let mut s = acp::ElicitationSchema::new();
                    s.properties.insert(
                        "response".into(),
                        acp::ElicitationPropertySchema::String(acp::StringPropertySchema::new()),
                    );
                    s
                });
                let mode = acp::ElicitationMode::Form(acp::ElicitationFormMode::new(form_schema));
                let req = acp::ElicitationRequest::new(sid.0.clone(), mode, prompt);
                let raw = serde_json::to_string(&req).unwrap_or_default();
                let raw_value = Arc::from(
                    serde_json::value::RawValue::from_string(raw)
                        .unwrap_or_else(|_| serde_json::value::RawValue::NULL.to_owned()),
                );
                let ext = acp::ExtRequest::new("elicitation/create", raw_value);
                let resp = conn.ext_method(ext).await;
                let text = match resp {
                    Ok(r) => {
                        let v: serde_json::Value =
                            serde_json::from_str(r.0.get()).unwrap_or_default();
                        if v.get("action").and_then(|a| a.as_str()) == Some("accept") {
                            v.get("content")
                                .and_then(|c| c.as_str())
                                .unwrap_or("")
                                .to_string()
                        } else {
                            String::new()
                        }
                    }
                    Err(_) => String::new(),
                };
                bridge
                    .respond_elicitation(&text, schema.as_ref(), &sid)
                    .await;
            }
            OutCmd::Done { sid } => {
                let mut comps = completions.lock().await;
                if let Some(tx) = comps.remove(&sid) {
                    let _ = tx.send(acp::StopReason::EndTurn);
                }
            }
            OutCmd::Error { sid } => {
                let mut comps = completions.lock().await;
                if let Some(tx) = comps.remove(&sid) {
                    let _ = tx.send(acp::StopReason::EndTurn);
                }
            }
        }
    }
}

// ── AgentlineAgent ───────────────────────────────────────────────────────

pub struct AgentlineAgent {
    bridge: Bridge,
    source: Arc<AcpSource>,
    source_id: String,
    sessions: RefCell<HashMap<acp::SessionId, SessionId>>,
}

impl AgentlineAgent {
    pub fn new(bridge: Bridge, source: Arc<AcpSource>) -> Self {
        let source_id = source.source_id.clone();
        Self {
            bridge,
            source,
            source_id,
            sessions: RefCell::new(HashMap::new()),
        }
    }
}

#[async_trait(?Send)]
impl acp::Agent for AgentlineAgent {
    async fn initialize(
        &self,
        _args: acp::InitializeRequest,
    ) -> acp::Result<acp::InitializeResponse> {
        let capabilities = acp::AgentCapabilities::new()
            .prompt_capabilities(acp::PromptCapabilities::new().image(true));
        Ok(acp::InitializeResponse::new(acp::ProtocolVersion::LATEST)
            .agent_info(acp::Implementation::new(
                "agentline",
                env!("CARGO_PKG_VERSION"),
            ))
            .agent_capabilities(capabilities))
    }

    async fn authenticate(
        &self,
        _args: acp::AuthenticateRequest,
    ) -> acp::Result<acp::AuthenticateResponse> {
        Err(acp::Error::method_not_found())
    }

    async fn new_session(
        &self,
        args: acp::NewSessionRequest,
    ) -> acp::Result<acp::NewSessionResponse> {
        let peer = PeerRef {
            user_id: "acp-client".into(),
            group_id: None,
            opaque: serde_json::Value::Null,
        };
        let sid = self
            .bridge
            .new_session(self.source_id.clone(), peer)
            .await
            .map_err(|e| acp::Error::new(-1, e.to_string()))?;

        if args.cwd.is_absolute() {
            self.bridge.set_cwd(args.cwd, Some(&sid)).await;
        }

        let acp_sid = acp::SessionId::new(sid.0.clone());
        self.sessions.borrow_mut().insert(acp_sid.clone(), sid);
        Ok(acp::NewSessionResponse::new(acp_sid))
    }

    async fn prompt(&self, args: acp::PromptRequest) -> acp::Result<acp::PromptResponse> {
        let sid = self
            .sessions
            .borrow()
            .get(&args.session_id)
            .cloned()
            .ok_or_else(|| acp::Error::new(-1, "unknown session"))?;

        let done_rx = self.source.register_completion(sid.clone()).await;

        self.bridge
            .prompt(sid, args.prompt)
            .await
            .map_err(|e| acp::Error::new(-1, e.to_string()))?;

        let stop = done_rx.await.unwrap_or(acp::StopReason::EndTurn);
        Ok(acp::PromptResponse::new(stop))
    }

    async fn cancel(&self, args: acp::CancelNotification) -> acp::Result<()> {
        let sid = self.sessions.borrow().get(&args.session_id).cloned();
        if let Some(sid) = sid {
            let _ = self.bridge.cancel(&sid).await;
        }
        Ok(())
    }

    async fn set_session_mode(
        &self,
        args: acp::SetSessionModeRequest,
    ) -> acp::Result<acp::SetSessionModeResponse> {
        let sid = self
            .sessions
            .borrow()
            .get(&args.session_id)
            .cloned()
            .ok_or_else(|| acp::Error::new(-1, "unknown session"))?;
        self.bridge.set_mode(&args.mode_id.0, Some(&sid)).await;
        Ok(acp::SetSessionModeResponse::new())
    }

    async fn close_session(
        &self,
        args: acp::CloseSessionRequest,
    ) -> acp::Result<acp::CloseSessionResponse> {
        let sid = self.sessions.borrow_mut().remove(&args.session_id);
        if let Some(sid) = sid {
            let _ = self.bridge.close_session(&sid).await;
        }
        Ok(acp::CloseSessionResponse::new())
    }

    async fn ext_method(&self, args: acp::ExtRequest) -> acp::Result<acp::ExtResponse> {
        match args.method.as_ref() {
            "providers/list" => {
                let result = self.bridge.list_providers().await;
                let json = match result {
                    Some((current, agents)) => {
                        let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
                        serde_json::json!({ "current": current, "available": names }).to_string()
                    }
                    None => "null".into(),
                };
                let raw = serde_json::value::RawValue::from_string(json)
                    .map_err(|e| acp::Error::new(-1, e.to_string()))?;
                Ok(acp::ExtResponse::new(Arc::from(raw)))
            }
            "providers/set" => {
                let v: serde_json::Value =
                    serde_json::from_str(args.params.get()).unwrap_or_default();
                let name = v
                    .get("name")
                    .and_then(|n| n.as_str())
                    .ok_or_else(|| acp::Error::new(-1, "missing \"name\" field"))?;
                self.bridge
                    .set_provider(name)
                    .await
                    .map_err(|e| acp::Error::new(-1, e.to_string()))?;
                let raw = serde_json::value::RawValue::from_string(r#"{"ok":true}"#.into())
                    .map_err(|e| acp::Error::new(-1, e.to_string()))?;
                Ok(acp::ExtResponse::new(Arc::from(raw)))
            }
            _ => Err(acp::Error::new(
                i32::from(acp::ErrorCode::MethodNotFound),
                format!("unsupported: {}", args.method),
            )),
        }
    }
}

// ── serve_acp entry point ────────────────────────────────────────────────

/// Start an ACP server on the given streams (single connection).
///
/// Must be called from within a `LocalSet` context (the ACP connection is
/// `!Send`). Blocks until the client disconnects.
pub async fn serve_acp(
    bridge: Bridge,
    source: Arc<AcpSource>,
    out_rx: mpsc::UnboundedReceiver<OutCmd>,
    stdin: impl tokio::io::AsyncRead + Unpin + 'static,
    stdout: impl tokio::io::AsyncWrite + Unpin + 'static,
) -> Result<()> {
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    let agent = AgentlineAgent::new(bridge.clone(), source.clone());

    let (conn, io_task) =
        acp::AgentSideConnection::new(agent, stdout.compat_write(), stdin.compat(), |fut| {
            tokio::task::spawn_local(fut);
        });

    let completions = source.completions.clone();
    tokio::task::spawn_local(process_outbound(conn, bridge, completions, out_rx));

    io_task
        .await
        .map_err(|e| crate::error::Error::other(format!("ACP server I/O error: {e}")))?;
    Ok(())
}
