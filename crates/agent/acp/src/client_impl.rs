//! `acp::Client` impl that forwards agent → host events into our internal
//! channels.
//!
//! Modeled after acp-cli's `BridgedAcpClient`. Notifications get mapped into
//! `AgentUpdate` and sent to the per-session update_tx; permission requests
//! park on a oneshot and return the resolved outcome.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use agent_client_protocol::{self as acp, Client};
use agentline_bridge::{
    AgentUpdate, ElicitField, ElicitFieldType, ElicitOption, PermissionDanger, ToolKind,
};
use serde_json::Value;
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::mapping;

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
    /// Atomically incrementing id for permission requests. Must be `!Send`
    /// only via the surrounding RefCell — the Client trait is `?Send` so
    /// we don't need atomic.
    perm_counter: RefCell<u64>,
}

impl BridgedClient {
    pub fn new(routing: Routing) -> Self {
        Self {
            routing,
            perm_counter: RefCell::new(0),
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
        let what = extract_what(&args.tool_call);
        let tool_kind = extract_tool_kind(&args.tool_call);

        // Push a PermissionRequest update to whichever stream owns this session.
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

        // Park here until the host (bridge) calls back with allow/deny.
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
        let updates = mapping::map_session_update(args.update);
        if updates.is_empty() {
            tracing::debug!("session update mapped to 0 AgentUpdates (ignored type)");
            return Ok(());
        }
        let streams = self.routing.session_streams.lock().await;
        match streams.get(&args.session_id) {
            Some(tx) => {
                for u in updates {
                    if tx.send(u).is_err() {
                        tracing::warn!(session=%args.session_id.0, "update_tx closed; dropping update");
                    }
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
            acp::ElicitationMode::Form(form) => {
                Some(map_elicitation_schema(&form.requested_schema))
            }
            _ => None,
        };

        // Push ElicitInput to the session stream.
        if let Some(tx) = self.routing.session_streams.lock().await.values().next() {
            let _ = tx.send(AgentUpdate::ElicitInput {
                id: elicit_id.clone(),
                prompt: req.message.clone(),
                schema,
            });
        }

        // Park until the bridge resolves the elicitation.
        let (resp_tx, resp_rx) = oneshot::channel();
        self.routing
            .pending_elicits
            .lock()
            .await
            .insert(elicit_id, resp_tx);
        let response = resp_rx.await.unwrap_or(Value::Null);

        // Build ACP ElicitationResponse as JSON.
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

fn map_elicitation_schema(schema: &acp::ElicitationSchema) -> Vec<ElicitField> {
    schema
        .properties
        .iter()
        .map(|(key, prop)| {
            let required = schema
                .required
                .as_ref()
                .map(|r| r.contains(key))
                .unwrap_or(false);
            let (title, description, field_type) = map_property_schema(prop);
            ElicitField {
                key: key.clone(),
                title,
                description,
                required,
                field_type,
            }
        })
        .collect()
}

fn map_property_schema(
    prop: &acp::ElicitationPropertySchema,
) -> (Option<String>, Option<String>, ElicitFieldType) {
    match prop {
        acp::ElicitationPropertySchema::String(s) => {
            let json = serde_json::to_value(s).unwrap_or_default();
            let title = json.get("title").and_then(|v| v.as_str()).map(String::from);
            let description = json
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from);

            // Check for enum (single-select)
            if let Some(values) = json.get("enum").and_then(|v| v.as_array()) {
                let options: Vec<ElicitOption> = values
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| ElicitOption {
                        value: s.to_string(),
                        label: s.to_string(),
                        description: None,
                    })
                    .collect();
                return (
                    title,
                    description,
                    ElicitFieldType::SingleSelect { options },
                );
            }

            // Check for oneOf (single-select with labels)
            if let Some(one_of) = json.get("oneOf").and_then(|v| v.as_array()) {
                let options: Vec<ElicitOption> = one_of
                    .iter()
                    .filter_map(|item| {
                        let value = item.get("const")?.as_str()?.to_string();
                        let label = item
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&value)
                            .to_string();
                        let desc = item
                            .get("description")
                            .and_then(|v| v.as_str())
                            .map(String::from);
                        Some(ElicitOption {
                            value,
                            label,
                            description: desc,
                        })
                    })
                    .collect();
                if !options.is_empty() {
                    return (
                        title,
                        description,
                        ElicitFieldType::SingleSelect { options },
                    );
                }
            }

            let format = json
                .get("format")
                .and_then(|v| v.as_str())
                .map(String::from);
            (title, description, ElicitFieldType::Text { format })
        }
        acp::ElicitationPropertySchema::Number(_n) => {
            let json = serde_json::to_value(_n).unwrap_or_default();
            let title = json.get("title").and_then(|v| v.as_str()).map(String::from);
            let description = json
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from);
            let minimum = json.get("minimum").and_then(|v| v.as_f64());
            let maximum = json.get("maximum").and_then(|v| v.as_f64());
            (
                title,
                description,
                ElicitFieldType::Number { minimum, maximum },
            )
        }
        acp::ElicitationPropertySchema::Integer(_i) => {
            let json = serde_json::to_value(_i).unwrap_or_default();
            let title = json.get("title").and_then(|v| v.as_str()).map(String::from);
            let description = json
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from);
            let minimum = json.get("minimum").and_then(|v| v.as_f64());
            let maximum = json.get("maximum").and_then(|v| v.as_f64());
            (
                title,
                description,
                ElicitFieldType::Number { minimum, maximum },
            )
        }
        acp::ElicitationPropertySchema::Boolean(_b) => {
            let json = serde_json::to_value(_b).unwrap_or_default();
            let title = json.get("title").and_then(|v| v.as_str()).map(String::from);
            let description = json
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from);
            (title, description, ElicitFieldType::Boolean)
        }
        acp::ElicitationPropertySchema::Array(ms) => {
            let json = serde_json::to_value(ms).unwrap_or_default();
            let title = json.get("title").and_then(|v| v.as_str()).map(String::from);
            let description = json
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from);

            // Multi-select: items.enum or items.oneOf
            let items = json.get("items").unwrap_or(&Value::Null);
            let mut options: Vec<ElicitOption> = Vec::new();

            if let Some(values) = items.get("enum").and_then(|v| v.as_array()) {
                options = values
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| ElicitOption {
                        value: s.to_string(),
                        label: s.to_string(),
                        description: None,
                    })
                    .collect();
            } else if let Some(one_of) = items.get("oneOf").and_then(|v| v.as_array()) {
                options = one_of
                    .iter()
                    .filter_map(|item| {
                        let value = item.get("const")?.as_str()?.to_string();
                        let label = item
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&value)
                            .to_string();
                        let desc = item
                            .get("description")
                            .and_then(|v| v.as_str())
                            .map(String::from);
                        Some(ElicitOption {
                            value,
                            label,
                            description: desc,
                        })
                    })
                    .collect();
            }

            (title, description, ElicitFieldType::MultiSelect { options })
        }
        _ => (None, None, ElicitFieldType::Text { format: None }),
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

/// Build the human-readable "what" string for a permission request.
/// Determine the ToolKind from a permission request's tool_call data.
fn extract_tool_kind(tool_call: &acp::ToolCallUpdate) -> ToolKind {
    let fields = &tool_call.fields;
    if let Some(k) = fields.kind.map(mapping::map_tool_kind) {
        return k;
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

///
/// `fields.title` is often truncated by the agent (e.g. middle-ellipsis on
/// long paths).  `fields.raw_input` always carries the complete, untruncated
/// parameters, so we try that first.
fn extract_what(tool_call: &acp::ToolCallUpdate) -> String {
    use agentline_bridge::{ToolKind, format::tool_label};

    let fields = &tool_call.fields;
    let mapped = fields.kind.map(mapping::map_tool_kind);

    if let Some(raw) = &fields.raw_input {
        // Shell / execute: raw_input = {"command": "..."}
        if let Some(cmd) = raw.get("command").and_then(|v| v.as_str()) {
            return tool_label(ToolKind::Shell, cmd);
        }

        // File operations: raw_input = {"path": "..."} or {"file_path": "..."}
        let path = raw
            .get("path")
            .or_else(|| raw.get("file_path"))
            .or_else(|| raw.get("filepath"))
            .and_then(|v| v.as_str());
        if let Some(path) = path {
            return tool_label(mapped.unwrap_or(ToolKind::FileRead), path);
        }

        // Web / fetch: raw_input = {"url": "..."}
        if let Some(url) = raw.get("url").and_then(|v| v.as_str()) {
            return tool_label(ToolKind::Web, url);
        }

        // Search: raw_input = {"pattern": "..."} or {"query": "..."}
        if let Some(pat) = raw
            .get("pattern")
            .or_else(|| raw.get("query"))
            .and_then(|v| v.as_str())
        {
            return tool_label(ToolKind::Search, pat);
        }

        // Fallback: compact JSON of the full input (untruncated, up to 2000 chars)
        if let Ok(json) = serde_json::to_string(raw) {
            if json.len() <= 2000 {
                return tool_label(mapped.unwrap_or(ToolKind::Other), &json);
            }
        }
    }

    // Last resort: title (may be truncated by the agent).
    match (mapped, fields.title.clone()) {
        (Some(k), Some(title)) => tool_label(k, &title),
        (_, Some(title)) => title,
        _ => "(未知工具)".to_string(),
    }
}
