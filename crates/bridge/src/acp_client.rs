//! `acp::Client` impl that forwards agent → host events into internal channels.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::{AgentUpdate, PermissionDanger, ToolKind};
use agent_client_protocol::{self as acp, Client};
use serde_json::Value;
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::acp_mapping as mapping;
use crate::driver::ToolCallParser;

pub struct Routing {
    pub session_streams: Arc<Mutex<HashMap<acp::SessionId, mpsc::UnboundedSender<AgentUpdate>>>>,
    pub pending_perms: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
    pub pending_elicits: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
}

impl Routing {
    pub fn with_pending(
        pending_perms: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
        pending_elicits: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
    ) -> Self {
        Self {
            session_streams: Arc::new(Mutex::new(HashMap::new())),
            pending_perms,
            pending_elicits,
        }
    }
}

pub struct BridgedClient {
    routing: Routing,
    perm_counter: RefCell<u64>,
    parser: Option<Arc<dyn ToolCallParser>>,
}

impl BridgedClient {
    pub fn new(routing: Routing, parser: Option<Arc<dyn ToolCallParser>>) -> Self {
        Self {
            routing,
            perm_counter: RefCell::new(0),
            parser,
        }
    }

    fn next_perm_id(&self) -> String {
        let mut c = self.perm_counter.borrow_mut();
        *c += 1;
        format!("perm-{}", *c)
    }
}

#[async_trait::async_trait(?Send)]
impl Client for BridgedClient {
    async fn request_permission(
        &self,
        args: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        let req_id = self.next_perm_id();
        if let Ok(raw) = serde_json::to_string(&args.tool_call) {
            tracing::debug!(tool_call=%raw, "permission request raw tool_call");
        }
        let parser = self.parser.as_deref();
        let effective_tc = match parser {
            Some(p) => p.enrich_permission(args.tool_call),
            None => args.tool_call,
        };
        let what = extract_what(parser, &effective_tc);
        let tool_kind = extract_tool_kind(parser, &effective_tc);

        if let Some(tx) = self
            .routing
            .session_streams
            .lock()
            .await
            .get(&args.session_id)
        {
            let _ = tx.send(AgentUpdate::PermissionRequest {
                id: req_id.clone(),
                what,
                danger: PermissionDanger::Medium,
                tool_kind,
            });
        }

        let (allow_tx, allow_rx) = oneshot::channel();
        self.routing
            .pending_perms
            .lock()
            .await
            .insert(req_id, allow_tx);
        let allow = allow_rx.await.unwrap_or(false);

        let outcome = pick_outcome(&args.options, allow);
        Ok(acp::RequestPermissionResponse::new(outcome))
    }

    async fn session_notification(&self, args: acp::SessionNotification) -> acp::Result<()> {
        tracing::debug!(session=%args.session_id.0, update=?args.update, "← ACP session_notification");
        if let Some(parser) = self.parser.as_deref() {
            match &args.update {
                acp::SessionUpdate::ToolCall(tc) => {
                    if let Ok(json) = serde_json::to_value(tc)
                        && let Ok(tcu) = serde_json::from_value::<acp::ToolCallUpdate>(json)
                    {
                        parser.observe(&tcu);
                    }
                }
                acp::SessionUpdate::ToolCallUpdate(tcu) => {
                    parser.observe(tcu);
                }
                _ => {}
            }
        }
        let streams = self.routing.session_streams.lock().await;
        match streams.get(&args.session_id) {
            Some(tx) => {
                if tx.send(AgentUpdate::Session(args.update)).is_err() {
                    tracing::warn!(session=%args.session_id.0, "update_tx closed; dropping update");
                }
            }
            None => {
                tracing::warn!(session=%args.session_id.0, "no stream registered for session; dropping updates");
            }
        }
        Ok(())
    }

    async fn ext_method(&self, args: acp::ExtRequest) -> acp::Result<acp::ExtResponse> {
        if args.method.as_ref() == "elicitation/create" {
            return self.handle_elicitation(args).await;
        }
        Err(acp::Error::new(
            i32::from(acp::ErrorCode::MethodNotFound),
            format!("unsupported ext method: {}", args.method),
        ))
    }
}

impl BridgedClient {
    async fn handle_elicitation(&self, args: acp::ExtRequest) -> acp::Result<acp::ExtResponse> {
        let req: acp::ElicitationRequest =
            serde_json::from_str(args.params.get()).map_err(|e| {
                acp::Error::new(i32::from(acp::ErrorCode::InvalidParams), e.to_string())
            })?;

        let elicit_id = format!("elicit-{}", {
            let mut c = self.perm_counter.borrow_mut();
            *c += 1;
            *c
        });

        let schema = match &req.mode {
            acp::ElicitationMode::Form(form) => Some(form.requested_schema.clone()),
            _ => None,
        };

        if let Some(tx) = self.routing.session_streams.lock().await.values().next() {
            let _ = tx.send(AgentUpdate::ElicitInput {
                id: elicit_id.clone(),
                prompt: req.message.clone(),
                schema,
            });
        }

        let (resp_tx, resp_rx) = oneshot::channel();
        self.routing
            .pending_elicits
            .lock()
            .await
            .insert(elicit_id, resp_tx);
        let response = resp_rx.await.unwrap_or(Value::Null);

        let action = if response.is_null() {
            serde_json::json!({ "action": "cancelled" })
        } else {
            serde_json::json!({ "action": "accept", "content": response })
        };
        let raw = serde_json::value::RawValue::from_string(action.to_string()).map_err(|e| {
            acp::Error::new(i32::from(acp::ErrorCode::InternalError), e.to_string())
        })?;
        Ok(acp::ExtResponse::new(Arc::from(raw)))
    }
}

fn pick_outcome(options: &[acp::PermissionOption], allow: bool) -> acp::RequestPermissionOutcome {
    let candidate = if allow {
        options.iter().find(|o| {
            matches!(
                o.kind,
                acp::PermissionOptionKind::AllowOnce | acp::PermissionOptionKind::AllowAlways
            )
        })
    } else {
        options.iter().find(|o| {
            matches!(
                o.kind,
                acp::PermissionOptionKind::RejectOnce | acp::PermissionOptionKind::RejectAlways
            )
        })
    };
    match candidate.or_else(|| options.first()) {
        Some(o) => acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(
            o.option_id.clone(),
        )),
        None => acp::RequestPermissionOutcome::Cancelled,
    }
}

fn extract_tool_kind(
    parser: Option<&dyn ToolCallParser>,
    tool_call: &acp::ToolCallUpdate,
) -> ToolKind {
    let generic = extract_tool_kind_generic(tool_call);
    if generic != ToolKind::Other {
        return generic;
    }
    parser
        .and_then(|p| p.refine_kind(tool_call))
        .unwrap_or(ToolKind::Other)
}

fn extract_tool_kind_generic(tool_call: &acp::ToolCallUpdate) -> ToolKind {
    let fields = &tool_call.fields;
    if let Some(k) = fields.kind.map(mapping::map_tool_kind)
        && k != ToolKind::Other
    {
        return k;
    }
    if let Some(title) = &fields.title
        && title.starts_with("mcp__")
    {
        return ToolKind::Mcp;
    }
    if let Some(raw) = &fields.raw_input {
        if raw.get("command").is_some() {
            return ToolKind::Shell;
        }
        if raw.get("path").is_some()
            || raw.get("file_path").is_some()
            || raw.get("filepath").is_some()
        {
            return ToolKind::FileEdit;
        }
        if raw.get("url").is_some() {
            return ToolKind::Web;
        }
        if raw.get("pattern").is_some() || raw.get("query").is_some() {
            return ToolKind::Search;
        }
    }
    ToolKind::Other
}

fn extract_what(parser: Option<&dyn ToolCallParser>, tool_call: &acp::ToolCallUpdate) -> String {
    use crate::format::tool_label;

    let fields = &tool_call.fields;
    let mapped = fields.kind.map(mapping::map_tool_kind);

    if let Some(raw) = &fields.raw_input {
        if let Some(cmd) = raw.get("command").and_then(|v| v.as_str()) {
            return tool_label(ToolKind::Shell, cmd);
        }
        let path = raw
            .get("path")
            .or_else(|| raw.get("file_path"))
            .or_else(|| raw.get("filepath"))
            .and_then(|v| v.as_str());
        if let Some(path) = path {
            return tool_label(mapped.unwrap_or(ToolKind::FileRead), path);
        }
        if let Some(url) = raw.get("url").and_then(|v| v.as_str()) {
            return tool_label(ToolKind::Web, url);
        }
        if let Some(pat) = raw
            .get("pattern")
            .or_else(|| raw.get("query"))
            .and_then(|v| v.as_str())
        {
            return tool_label(ToolKind::Search, pat);
        }
        if let Ok(json) = serde_json::to_string(raw)
            && json.len() <= 2000
        {
            return tool_label(mapped.unwrap_or(ToolKind::Other), &json);
        }
    }

    if let Some(w) = parser.and_then(|p| p.refine_what(tool_call)) {
        return w;
    }

    match (mapped, fields.title.clone()) {
        (Some(k), Some(title)) => tool_label(k, &title),
        (_, Some(title)) => title,
        _ => "(未知工具)".to_string(),
    }
}
