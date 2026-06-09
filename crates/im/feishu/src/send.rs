use crate::auth::TokenManager;
use crate::error::{Error, Result};
use crate::types::SendMessageResp;
use std::time::Duration;

const SEND_MSG_URL: &str =
    "https://open.feishu.cn/open-apis/im/v1/messages?receive_id_type=open_id";
const MSG_BASE_URL: &str = "https://open.feishu.cn/open-apis/im/v1/messages";

pub async fn send_text(
    http: &reqwest::Client,
    token_mgr: &TokenManager,
    open_id: &str,
    text: &str,
) -> Result<String> {
    let content = serde_json::json!({"text": text}).to_string();
    send_message(http, token_mgr, open_id, "text", &content).await
}

pub async fn send_post(
    http: &reqwest::Client,
    token_mgr: &TokenManager,
    open_id: &str,
    title: &str,
    text: &str,
) -> Result<String> {
    let lines: Vec<Vec<serde_json::Value>> = text
        .lines()
        .map(|line| vec![serde_json::json!({"tag": "text", "text": line})])
        .collect();
    let content = serde_json::json!({
        "zh_cn": {
            "title": title,
            "content": lines,
        }
    })
    .to_string();
    send_message(http, token_mgr, open_id, "post", &content).await
}

pub async fn send_card(
    http: &reqwest::Client,
    token_mgr: &TokenManager,
    open_id: &str,
    card_json: &str,
) -> Result<String> {
    send_message(http, token_mgr, open_id, "interactive", card_json).await
}

pub async fn update_card(
    http: &reqwest::Client,
    token_mgr: &TokenManager,
    message_id: &str,
    card_json: &str,
) -> Result<()> {
    let token = token_mgr.token().await;
    let url = format!("{MSG_BASE_URL}/{message_id}");
    let body = serde_json::json!({
        "msg_type": "interactive",
        "content": card_json,
    });
    let resp = http
        .patch(&url)
        .timeout(Duration::from_secs(15))
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::http(format!("PATCH {url}: {e}")))?;
    check_response(resp).await?;
    Ok(())
}

async fn send_message(
    http: &reqwest::Client,
    token_mgr: &TokenManager,
    open_id: &str,
    msg_type: &str,
    content: &str,
) -> Result<String> {
    let token = token_mgr.token().await;
    let body = serde_json::json!({
        "receive_id": open_id,
        "msg_type": msg_type,
        "content": content,
    });
    let resp = http
        .post(SEND_MSG_URL)
        .timeout(Duration::from_secs(15))
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::http(format!("send_{msg_type}: {e}")))?;
    check_response(resp).await
}

async fn check_response(resp: reqwest::Response) -> Result<String> {
    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| Error::http(format!("read body: {e}")))?;
    if !status.is_success() {
        return Err(Error::Api(format!(
            "feishu api → {}: {}",
            status,
            String::from_utf8_lossy(&bytes)
        )));
    }
    let parsed: SendMessageResp =
        serde_json::from_slice(&bytes).map_err(|e| Error::Parse(format!("send resp: {e}")))?;
    if parsed.code != 0 {
        return Err(Error::Api(format!(
            "feishu send code={}: {}",
            parsed.code, parsed.msg
        )));
    }
    Ok(parsed
        .data
        .map(|d| d.message_id)
        .unwrap_or_default())
}
