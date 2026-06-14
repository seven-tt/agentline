use crate::agent::AgentBackend;
use crate::bridge::{BridgeCommand, BridgeConfig};
use crate::error::Result;
use crate::event::{OutboundEvent, ToolEvent};
use crate::permission::{PendingPerm, PermResponse, PermissionDecision};
use crate::router::SourceRouter;
use crate::session::SessionKey;
use crate::state::PendingElicit;
use crate::types::{AgentUpdate, PeerRef, SessionId, ToolKind};
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_prompt_task_routed(
    router: Arc<SourceRouter>,
    source_id: String,
    agent: Arc<dyn AgentBackend>,
    actor_tx: mpsc::Sender<BridgeCommand>,
    cfg: BridgeConfig,
    key: SessionKey,
    peer: PeerRef,
    sid: SessionId,
    tag: String,
    text: String,
) -> Result<()> {
    let (cwd_tx, cwd_rx) = oneshot::channel();
    let _ = actor_tx
        .send(BridgeCommand::GetSessionCwd {
            key: key.clone(),
            reply: cwd_tx,
        })
        .await;
    let target_cwd = cwd_rx.await.unwrap_or_else(|_| cfg.default_cwd.clone());

    let send = |event: OutboundEvent| {
        let router = router.clone();
        let source_id = source_id.clone();
        let peer = peer.clone();
        async move {
            if let Err(e) = router.send_event(&source_id, &peer, &event).await {
                tracing::error!(error=%e, "send_event failed");
            }
        }
    };

    let result: Result<()> = async {
        tracing::debug!(session=%sid.as_str(), "→ sending prompt to agent");
        let mut stream = agent.prompt(&sid, &text).await?;
        tracing::debug!(session=%sid.as_str(), "agent stream opened; relaying updates");

        let mut thinking_start: Option<std::time::Instant> = None;
        let mut reply_buf = String::new();
        let mut reply_send_ok = true;
        let mut stream_started = false;
        let mut tool_meta: HashMap<String, (ToolKind, String)> = HashMap::new();
        let mut tool_ended: std::collections::HashSet<String> = std::collections::HashSet::new();
        let perm_just_denied = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut has_visible_output = false;

        while let Some(update) = stream.next().await {
            tracing::debug!(update=?update, "← agent update");

            if !matches!(
                update,
                AgentUpdate::Thinking { .. }
                    | AgentUpdate::ToolCallStart { .. }
                    | AgentUpdate::ToolCallEnd { .. }
                    | AgentUpdate::ToolCallProgress { .. }
            ) && thinking_start.is_some()
            {
                let elapsed = thinking_start
                    .take()
                    .map(|s| s.elapsed().as_secs_f64())
                    .unwrap_or(0.0);
                send(OutboundEvent::ThinkingEnd {
                    tag: tag.clone(),
                    elapsed_secs: elapsed,
                })
                .await;
            }

            match update {
                AgentUpdate::Thinking { text } => {
                    if thinking_start.is_none() {
                        thinking_start = Some(std::time::Instant::now());
                    }
                    send(OutboundEvent::Thinking {
                        tag: tag.clone(),
                        text,
                    })
                    .await;
                }

                AgentUpdate::AssistantText { delta, is_final } => {
                    if !delta.is_empty() {
                        has_visible_output = true;
                        if !stream_started {
                            stream_started = true;
                            send(OutboundEvent::StreamStart { tag: tag.clone() }).await;
                        }
                        reply_buf.push_str(&delta);
                        if let Err(e) = router
                            .send_event(
                                &source_id,
                                &peer,
                                &OutboundEvent::StreamChunk { text: delta },
                            )
                            .await
                        {
                            reply_send_ok = false;
                            tracing::error!(error=%e, "send_event StreamChunk failed");
                        }
                    }
                    if is_final
                        && let Err(e) = router
                            .send_event(&source_id, &peer, &OutboundEvent::StreamEnd)
                            .await
                    {
                        reply_send_ok = false;
                        tracing::error!(error=%e, "send_event StreamEnd failed");
                    }
                }

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
                            key: key.clone(),
                            tool_kind,
                            what: what.clone(),
                            reply: dec_tx,
                        })
                        .await;
                    let decision = dec_rx.await.unwrap_or(PermissionDecision::Ask);

                    match decision {
                        PermissionDecision::AutoApprove(reason) => {
                            if let Err(e) = agent.answer_permission(&sid, &id, true).await {
                                tracing::error!(error=%e, "auto-approve failed");
                            }
                            tracing::debug!(what=%what, ?reason, "auto-approve (silent)");
                        }
                        PermissionDecision::Ask => {
                            let (tx, rx) = oneshot::channel::<PermResponse>();
                            let _ = actor_tx
                                .send(BridgeCommand::InsertPendingPerm {
                                    key: key.clone(),
                                    perm: PendingPerm {
                                        session_id: sid.clone(),
                                        request_id: id.clone(),
                                        responder: tx,
                                        peer: peer.clone(),
                                        tool_kind,
                                        what: what.clone(),
                                    },
                                })
                                .await;
                            let agent_c = agent.clone();
                            let sid_c = sid.clone();
                            let req_c = id.clone();
                            let denied_flag = perm_just_denied.clone();
                            tokio::spawn(async move {
                                let resp = rx.await.unwrap_or(PermResponse::Deny);
                                let allow = !matches!(resp, PermResponse::Deny);
                                if !allow {
                                    denied_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                                }
                                if let Err(e) =
                                    agent_c.answer_permission(&sid_c, &req_c, allow).await
                                {
                                    tracing::error!(error=%e, "answer_permission failed");
                                }
                            });
                            send(OutboundEvent::PermissionRequest {
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
                            key: key.clone(),
                            elicit: PendingElicit {
                                session_id: sid.clone(),
                                elicit_id: id.clone(),
                                peer: peer.clone(),
                                schema: schema.clone(),
                            },
                        })
                        .await;
                    send(OutboundEvent::ElicitInput { id, prompt, schema }).await;
                }

                AgentUpdate::ModeChanged { mode_id } => {
                    send(OutboundEvent::ModeChanged {
                        tag: tag.clone(),
                        mode_id,
                    })
                    .await;
                }

                AgentUpdate::SessionInfo { title } => {
                    send(OutboundEvent::SessionTitle {
                        tag: tag.clone(),
                        title,
                    })
                    .await;
                }

                AgentUpdate::Done => {
                    if !reply_buf.is_empty() {
                        tracing::info!(
                            peer = %peer.user_id,
                            ok = reply_send_ok,
                            text = %reply_buf,
                            "→ outbound reply"
                        );
                    }
                    send(OutboundEvent::StreamEnd).await;
                    send(OutboundEvent::Done {
                        silent: !has_visible_output,
                    })
                    .await;
                    break;
                }

                AgentUpdate::ToolCallStart { id, kind, label } => {
                    let label = crate::format::strip_cwd_prefix(&label, &target_cwd);
                    tool_meta.insert(id.clone(), (kind, label.clone()));
                    send(OutboundEvent::Tool(ToolEvent::Start { id, kind, label })).await;
                }
                AgentUpdate::ToolCallProgress { id, output_chunk } => {
                    send(OutboundEvent::Tool(ToolEvent::Progress {
                        id,
                        output: output_chunk,
                    }))
                    .await;
                }
                AgentUpdate::ToolCallEnd { id, ok, summary } => {
                    if !tool_ended.insert(id.clone()) {
                        tracing::debug!(id=%id, "suppressing duplicate tool-end");
                        continue;
                    }
                    if !ok {
                        perm_just_denied.store(false, std::sync::atomic::Ordering::SeqCst);
                    }
                    let (kind, label) = tool_meta
                        .remove(&id)
                        .unwrap_or((ToolKind::Other, String::new()));
                    let summary = summary.map(|s| {
                        if s.starts_with("```") {
                            let inner = s.trim_start_matches('`').trim_end_matches('`').trim();
                            format!("```\n{inner}\n```")
                        } else if !ok && !s.is_empty() {
                            crate::format::truncate(&s, 800)
                        } else {
                            s
                        }
                    });
                    send(OutboundEvent::Tool(ToolEvent::End {
                        id,
                        ok,
                        summary,
                        kind,
                        label,
                    }))
                    .await;
                }
                AgentUpdate::Plan { steps } => {
                    send(OutboundEvent::Plan { steps }).await;
                }
                AgentUpdate::Error(msg) => {
                    send(OutboundEvent::Error(msg)).await;
                }
            }
        }
        tracing::debug!(session=%sid.as_str(), "✓ prompt completed");
        Ok(())
    }
    .await;

    result
}
