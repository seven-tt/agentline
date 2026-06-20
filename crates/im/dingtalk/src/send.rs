use crate::auth::{DEFAULT_OPEN_API_HOST, TokenManager};
use crate::error::{Error, Result};
use crate::stream::WebhookCache;
use crate::types::{
    CardData, CreateAndDeliverReq, ImGroupOpenDeliverModel, ImGroupOpenSpaceModel,
    ImRobotOpenDeliverModel, ImRobotOpenSpaceModel, SessionWebhookMarkdown, SessionWebhookText,
    StreamingUpdateReq,
};
use agentline_im_core::types::PeerRef;
use std::time::Duration;

const CREATE_AND_DELIVER_PATH: &str = "/v1.0/card/instances/createAndDeliver";
const STREAMING_UPDATE_PATH: &str = "/v1.0/card/streaming";

/// POST a JSON body to the most recent `sessionWebhook` cached for `to`.
///
/// DingTalk webhooks expire after a few minutes of inactivity; if we run out
/// of fresh webhook this returns `Error::Api(...)` and the caller can decide
/// whether to fall back to the work-notification API.
async fn post_webhook(
    http: &reqwest::Client,
    webhooks: &WebhookCache,
    to: &PeerRef,
    body: &impl serde::Serialize,
) -> Result<()> {
    let webhook = webhooks
        .lock()
        .await
        .get(&to.user_id)
        .cloned()
        .ok_or_else(|| Error::Api(format!("no session webhook cached for {}", to.user_id)))?;

    let resp = http
        .post(&webhook)
        .timeout(Duration::from_secs(15))
        .json(body)
        .send()
        .await
        .map_err(|e| Error::http(format!("POST session_webhook: {e}")))?;
    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| Error::http(format!("read body: {e}")))?;
    if !status.is_success() {
        return Err(Error::Api(format!(
            "session webhook → {}: {}",
            status,
            String::from_utf8_lossy(&bytes)
        )));
    }
    Ok(())
}

/// Reply with a plain-text webhook message.
pub async fn send_text(
    http: &reqwest::Client,
    webhooks: &WebhookCache,
    to: &PeerRef,
    text: &str,
) -> Result<()> {
    post_webhook(http, webhooks, to, &SessionWebhookText::new(text)).await
}

/// Reply with a `msgtype: "markdown"` webhook message. Supports headers,
/// bold/italic, links, images, lists and quotes — not GFM tables.
pub async fn send_markdown(
    http: &reqwest::Client,
    webhooks: &WebhookCache,
    to: &PeerRef,
    title: &str,
    text: &str,
) -> Result<()> {
    post_webhook(
        http,
        webhooks,
        to,
        &SessionWebhookMarkdown::new(title, text),
    )
    .await
}

// ── Card APIs ─────────────────────────────────────────────────────

pub struct CardDeliverParams<'a> {
    pub card_template_id: &'a str,
    pub out_track_id: &'a str,
    pub robot_code: &'a str,
    pub initial_content: &'a str,
    pub flow_status: &'a str,
}

#[allow(clippy::too_many_arguments)]
pub async fn create_and_deliver_card(
    http: &reqwest::Client,
    token_mgr: &TokenManager,
    peer: &PeerRef,
    params: &CardDeliverParams<'_>,
) -> Result<()> {
    let is_group = peer.group_id.is_some();
    let open_space_id = if is_group {
        let conv_id = peer
            .opaque
            .get("conversation_id")
            .and_then(|v| v.as_str())
            .unwrap_or(&peer.user_id);
        format!("dtv1.card//IM_GROUP.{conv_id}")
    } else {
        format!("dtv1.card//IM_ROBOT.{}", peer.user_id)
    };

    let mut card_params = std::collections::HashMap::new();
    card_params.insert("content".into(), params.initial_content.into());
    card_params.insert("flowStatus".into(), params.flow_status.into());

    let req = CreateAndDeliverReq {
        card_template_id: params.card_template_id.into(),
        out_track_id: params.out_track_id.into(),
        callback_type: "STREAM".into(),
        card_data: CardData {
            card_param_map: card_params,
        },
        open_space_id,
        user_id_type: 1,
        im_robot_open_deliver_model: if is_group {
            None
        } else {
            Some(ImRobotOpenDeliverModel {
                space_type: "IM_ROBOT".into(),
            })
        },
        im_group_open_deliver_model: if is_group {
            Some(ImGroupOpenDeliverModel {
                robot_code: params.robot_code.into(),
            })
        } else {
            None
        },
        im_robot_open_space_model: if is_group {
            None
        } else {
            Some(ImRobotOpenSpaceModel {
                support_forward: true,
            })
        },
        im_group_open_space_model: if is_group {
            Some(ImGroupOpenSpaceModel {
                support_forward: true,
            })
        } else {
            None
        },
    };

    let token = token_mgr.token().await;
    let url = format!("{DEFAULT_OPEN_API_HOST}{CREATE_AND_DELIVER_PATH}");
    let resp = http
        .post(&url)
        .timeout(Duration::from_secs(15))
        .header("x-acs-dingtalk-access-token", &token)
        .json(&req)
        .send()
        .await
        .map_err(|e| Error::http(format!("POST createAndDeliver: {e}")))?;
    check_api_response(resp, "createAndDeliver").await
}

pub struct StreamUpdate {
    pub is_full: bool,
    pub is_finalize: bool,
    pub is_error: bool,
}

pub async fn streaming_update(
    http: &reqwest::Client,
    token_mgr: &TokenManager,
    out_track_id: &str,
    key: &str,
    content: &str,
    flags: &StreamUpdate,
) -> Result<()> {
    let req = StreamingUpdateReq {
        out_track_id: out_track_id.into(),
        guid: gen_track_id(),
        key: key.into(),
        content: content.into(),
        is_full: flags.is_full,
        is_finalize: flags.is_finalize,
        is_error: flags.is_error,
    };

    let token = token_mgr.token().await;
    let url = format!("{DEFAULT_OPEN_API_HOST}{STREAMING_UPDATE_PATH}");
    let resp = http
        .put(&url)
        .timeout(Duration::from_secs(15))
        .header("x-acs-dingtalk-access-token", &token)
        .json(&req)
        .send()
        .await
        .map_err(|e| Error::http(format!("PUT streaming: {e}")))?;
    check_api_response(resp, "streaming").await
}

async fn check_api_response(resp: reqwest::Response, label: &str) -> Result<()> {
    let status = resp.status();
    if !status.is_success() {
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::http(format!("read body: {e}")))?;
        return Err(Error::Api(format!(
            "{label} → {}: {}",
            status,
            String::from_utf8_lossy(&bytes)
        )));
    }
    Ok(())
}

/// Generate a unique tracking ID (hex-encoded timestamp + random bytes).
pub fn gen_track_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let rand: u64 = {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        let s = RandomState::new();
        let mut h = s.build_hasher();
        h.write_u128(ts);
        h.finish()
    };
    format!("{:016x}{:016x}", ts as u64, rand)
}
