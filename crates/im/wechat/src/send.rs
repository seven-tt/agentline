use crate::error::{Error, Result};
use crate::http::HttpClient;
use crate::types::{
    BaseInfo, GetConfigReq, GetConfigResp, Item, SendMessageReq, SendMessageResp, SendTypingReq,
    StreamSignalItem, WeixinMessage,
};
use agentline_im_core::types::PeerRef;

/// Unique per-message id matching openclaw's format: `{prefix}:{ms}-{hex8}`.
fn generate_client_id() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let rand: u32 = rand::random();
    format!("agentline:{ms}-{rand:08x}")
}

/// Extract the iLink `context_token` that the bridge stashed into `peer.opaque`
/// during ingest. Without it the reply lands in the wrong conversation slot.
pub fn extract_context_token(peer: &PeerRef) -> Option<String> {
    peer.opaque
        .get("context_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

pub async fn send_text(http: &HttpClient, to: &PeerRef, text: &str) -> Result<()> {
    let context_token = extract_context_token(to)
        .ok_or_else(|| Error::other("peer.opaque is missing context_token"))?;
    let req = SendMessageReq {
        msg: WeixinMessage {
            from_user_id: String::new(),
            to_user_id: to.user_id.clone(),
            message_type: 2,
            message_state: 2,
            context_token,
            client_id: Some(generate_client_id()),
            // Omit group_id entirely for 1:1 chats. An empty-string group_id
            // makes the server treat it as a (malformed) group message → ret=-2.
            group_id: to.group_id.clone().filter(|g| !g.is_empty()),
            item_list: vec![Item::text(text)],
            extra: Default::default(),
        },
        base_info: BaseInfo::current(),
    };
    if let Ok(body) = serde_json::to_string(&req) {
        tracing::debug!(body = %body, "→ sendmessage request");
    }
    let resp: SendMessageResp = http.post_json("/ilink/bot/sendmessage", &req).await?;
    if resp.ret != 0 {
        tracing::error!(ret = resp.ret, msg = ?resp.errmsg, "sendmessage rejected");
        return Err(Error::Api {
            ret: resp.ret,
            msg: resp.errmsg.unwrap_or_default(),
        });
    }
    Ok(())
}

/// Send a single structured MessageItem.
pub async fn send_message_item(http: &HttpClient, to: &PeerRef, item: Item) -> Result<()> {
    let context_token = extract_context_token(to)
        .ok_or_else(|| Error::other("peer.opaque is missing context_token"))?;
    let req = SendMessageReq {
        msg: WeixinMessage {
            from_user_id: String::new(),
            to_user_id: to.user_id.clone(),
            message_type: 2,
            message_state: 2,
            context_token,
            client_id: Some(generate_client_id()),
            group_id: to.group_id.clone().filter(|g| !g.is_empty()),
            item_list: vec![item],
            extra: Default::default(),
        },
        base_info: BaseInfo::current(),
    };
    let resp: SendMessageResp = http.post_json("/ilink/bot/sendmessage", &req).await?;
    if resp.ret != 0 {
        tracing::error!(ret = resp.ret, msg = ?resp.errmsg, "sendmessage rejected");
        return Err(Error::Api {
            ret: resp.ret,
            msg: resp.errmsg.unwrap_or_default(),
        });
    }
    Ok(())
}

/// Send the stream_start signal message that creates the renderer node.
pub async fn send_stream_signal(
    http: &HttpClient,
    to: &PeerRef,
    stream_ticket: &str,
    stream_id: &str,
    action: &str,
) -> Result<()> {
    let context_token = extract_context_token(to)
        .ok_or_else(|| Error::other("peer.opaque is missing context_token"))?;
    let item = Item {
        item_type: 14, // STREAM_SIGNAL
        text_item: None,
        image_item: None,
        voice_item: None,
        file_item: None,
        video_item: None,
        tool_call_start_item: None,
        tool_call_result_item: None,
        thinking_item: None,
        stream_signal_item: Some(StreamSignalItem {
            stream_type: "text".to_string(),
            stream_id: stream_id.to_string(),
            ilink_stream_ticket: stream_ticket.to_string(),
            digest: None,
            action: action.to_string(),
        }),
    };
    let req = SendMessageReq {
        msg: WeixinMessage {
            from_user_id: String::new(),
            to_user_id: to.user_id.clone(),
            message_type: 2,
            message_state: 2,
            context_token,
            client_id: Some(generate_client_id()),
            group_id: to.group_id.clone().filter(|g| !g.is_empty()),
            item_list: vec![item],
            extra: Default::default(),
        },
        base_info: BaseInfo::current(),
    };
    let resp: SendMessageResp = http.post_json("/ilink/bot/sendmessage", &req).await?;
    if resp.ret != 0 {
        tracing::error!(ret = resp.ret, msg = ?resp.errmsg, "send_stream_signal rejected");
        return Err(Error::Api {
            ret: resp.ret,
            msg: resp.errmsg.unwrap_or_default(),
        });
    }
    Ok(())
}

/// Best-effort typing indicator. Requires a typing_ticket fetched from
/// /getconfig per (peer, context_token). On failure we log and return Ok —
/// typing is a courtesy, not a correctness signal.
pub async fn send_typing(http: &HttpClient, to: &PeerRef, status: i32) -> Result<()> {
    let context_token = match extract_context_token(to) {
        Some(c) => c,
        None => return Ok(()),
    };
    let cfg_req = GetConfigReq {
        ilink_user_id: to.user_id.clone(),
        context_token,
        base_info: BaseInfo::current(),
    };
    let cfg: GetConfigResp = match http.post_json("/ilink/bot/getconfig", &cfg_req).await {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(error=%e, "getconfig failed; skipping typing");
            return Ok(());
        }
    };
    let Some(ticket) = cfg.typing_ticket else {
        tracing::debug!(user=%to.user_id, "getconfig returned no typing_ticket");
        return Ok(());
    };
    let typing_req = SendTypingReq {
        ilink_user_id: to.user_id.clone(),
        typing_ticket: ticket,
        status,
        base_info: BaseInfo::current(),
    };
    let _: serde_json::Value = match http.post_json("/ilink/bot/sendtyping", &typing_req).await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(error=%e, "sendtyping failed (non-fatal)");
            return Ok(());
        }
    };
    Ok(())
}
