use crate::error::{Error, Result};
use crate::types::{EditMessageTextReq, SendChatActionReq, SendMessageReq, SendMessageResp};
use std::time::Duration;

pub async fn send_message(
    http: &reqwest::Client,
    api_base: &str,
    token: &str,
    chat_id: i64,
    text: &str,
) -> Result<i64> {
    let url = format!("{api_base}/bot{token}/sendMessage");
    let body = SendMessageReq {
        chat_id,
        text: text.to_string(),
        parse_mode: None,
    };
    let resp = http
        .post(&url)
        .timeout(Duration::from_secs(15))
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::http(format!("sendMessage: {e}")))?;
    parse_send_response(resp).await
}

pub async fn edit_message_text(
    http: &reqwest::Client,
    api_base: &str,
    token: &str,
    chat_id: i64,
    message_id: i64,
    text: &str,
) -> Result<()> {
    let url = format!("{api_base}/bot{token}/editMessageText");
    let body = EditMessageTextReq {
        chat_id,
        message_id,
        text: text.to_string(),
        parse_mode: None,
    };
    let resp = http
        .post(&url)
        .timeout(Duration::from_secs(15))
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::http(format!("editMessageText: {e}")))?;
    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| Error::http(format!("read body: {e}")))?;
    if !status.is_success() {
        return Err(Error::Api(format!(
            "editMessageText → {}: {}",
            status,
            String::from_utf8_lossy(&bytes)
        )));
    }
    Ok(())
}

pub async fn send_chat_action(
    http: &reqwest::Client,
    api_base: &str,
    token: &str,
    chat_id: i64,
    action: &str,
) -> Result<()> {
    let url = format!("{api_base}/bot{token}/sendChatAction");
    let body = SendChatActionReq {
        chat_id,
        action: action.to_string(),
    };
    let _ = http
        .post(&url)
        .timeout(Duration::from_secs(10))
        .json(&body)
        .send()
        .await;
    Ok(())
}

async fn parse_send_response(resp: reqwest::Response) -> Result<i64> {
    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| Error::http(format!("read body: {e}")))?;
    if !status.is_success() {
        return Err(Error::Api(format!(
            "sendMessage → {}: {}",
            status,
            String::from_utf8_lossy(&bytes)
        )));
    }
    let parsed: SendMessageResp = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Parse(format!("sendMessage resp: {e}")))?;
    if !parsed.ok {
        return Err(Error::Api(
            parsed
                .description
                .unwrap_or_else(|| "unknown error".into()),
        ));
    }
    Ok(parsed.result.map(|r| r.message_id).unwrap_or(0))
}
