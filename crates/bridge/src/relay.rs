use crate::agent::AgentBackend;
use crate::bridge::{BridgeCommand, BridgeConfig};
use crate::error::Result;
use crate::event::AgentEvent;
use crate::permission::{PendingPermissionRequest, PermissionDecision, PermissionResponse};
use crate::router::SourceRouter;
use crate::session::ChannelBinding;
use crate::state::PendingElicit;
use crate::types::{AgentSessionId, AgentUpdate, ContentBlock, SessionId};
use agent_client_protocol::SessionUpdate;
use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

/// Drive one prompt turn: read the agent's [`AgentUpdate`] stream, apply the
/// permission policy (auto-approve / ask), persist pending perm/elicit state,
/// and forward every semantic update to the session's channels as an
/// [`AgentEvent`]. No presentation is synthesized here — that is the rendering
/// layer's job.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_prompt_task_routed(
    router: Arc<SourceRouter>,
    agent: Arc<dyn AgentBackend>,
    actor_tx: mpsc::Sender<BridgeCommand>,
    cfg: BridgeConfig,
    session_id: SessionId,
    bindings: Vec<ChannelBinding>,
    agent_session_id: AgentSessionId,
    tag: String,
    target_cwd: std::path::PathBuf,
    content: Vec<ContentBlock>,
) -> Result<()> {
    let _ = cfg;
    // Forward one semantic update to every channel the session is bound to.
    let emit = |update: AgentUpdate| {
        let router = router.clone();
        let bindings = bindings.clone();
        let session_id = session_id.clone();
        let tag = tag.clone();
        async move {
            let ev = AgentEvent::new(session_id, tag, update);
            for b in &bindings {
                if let Err(e) = router.send_update(&b.source_id, &b.peer, &ev).await {
                    tracing::error!(error=%e, "send_update failed");
                }
            }
        }
    };

    let result: Result<()> = async {
        tracing::debug!(session=%agent_session_id, "→ sending prompt to agent");
        let mut stream = agent.prompt(&agent_session_id, &content).await?;
        tracing::debug!(session=%agent_session_id, "agent stream opened; relaying updates");

        while let Some(update) = stream.next().await {
            tracing::debug!(update=?update, "← agent update");

            match update {
                AgentUpdate::PermissionRequest {
                    id,
                    what: raw_what,
                    danger,
                    tool_kind,
                } => {
                    let cwd_s = target_cwd.to_string_lossy();
                    let what = raw_what.replace(cwd_s.as_ref(), ".");

                    let (dec_tx, dec_rx) = oneshot::channel();
                    let _ = actor_tx
                        .send(BridgeCommand::GetPermDecision {
                            session_id: session_id.clone(),
                            tool_kind,
                            what: what.clone(),
                            reply: dec_tx,
                        })
                        .await;
                    let decision = dec_rx.await.unwrap_or(PermissionDecision::Ask);

                    match decision {
                        PermissionDecision::AutoApprove(reason) => {
                            if let Err(e) =
                                agent.respond_permission(&agent_session_id, &id, true).await
                            {
                                tracing::error!(error=%e, "auto-approve failed");
                            }
                            tracing::debug!(what=%what, ?reason, "auto-approve (silent)");
                        }
                        PermissionDecision::Ask => {
                            let (tx, rx) = oneshot::channel::<PermissionResponse>();
                            let _ = actor_tx
                                .send(BridgeCommand::InsertPendingPermissionRequest {
                                    session_id: session_id.clone(),
                                    perm: PendingPermissionRequest {
                                        agent_session_id: agent_session_id.clone(),
                                        request_id: id.clone(),
                                        responder: tx,
                                        peer: bindings[0].peer.clone(),
                                        tool_kind,
                                        what: what.clone(),
                                    },
                                })
                                .await;
                            let agent_c = agent.clone();
                            let asid_c = agent_session_id.clone();
                            let req_c = id.clone();
                            tokio::spawn(async move {
                                let resp = rx.await.unwrap_or(PermissionResponse::Deny);
                                let allow = !matches!(resp, PermissionResponse::Deny);
                                if let Err(e) =
                                    agent_c.respond_permission(&asid_c, &req_c, allow).await
                                {
                                    tracing::error!(error=%e, "respond_permission failed");
                                }
                            });
                            emit(AgentUpdate::PermissionRequest {
                                id,
                                what,
                                danger,
                                tool_kind,
                            })
                            .await;
                        }
                    }
                }

                AgentUpdate::ElicitInput { id, prompt, schema } => {
                    let _ = actor_tx
                        .send(BridgeCommand::InsertPendingElicit {
                            session_id: session_id.clone(),
                            elicit: PendingElicit {
                                agent_session_id: agent_session_id.clone(),
                                elicit_id: id.clone(),
                                peer: bindings[0].peer.clone(),
                                schema: schema.clone(),
                            },
                        })
                        .await;
                    emit(AgentUpdate::ElicitInput { id, prompt, schema }).await;
                }

                AgentUpdate::Session(SessionUpdate::ToolCall(mut tc)) => {
                    tc.title = crate::format::strip_cwd_prefix(&tc.title, &target_cwd);
                    emit(AgentUpdate::Session(SessionUpdate::ToolCall(tc))).await;
                }

                AgentUpdate::Done => {
                    emit(AgentUpdate::Done).await;
                    break;
                }

                other => {
                    emit(other).await;
                }
            }
        }
        tracing::debug!(session=%agent_session_id, "✓ prompt completed");
        Ok(())
    }
    .await;

    result
}
