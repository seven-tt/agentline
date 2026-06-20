use crate::error::{Error, Result};
use crate::types::{
    EditMessageTextReq, InlineKeyboardMarkup, SendChatActionReq, SendMessageReq, SendMessageResp,
};
use std::time::Duration;

pub async fn send_message(
    http: &reqwest::Client,
    api_base: &str,
    token: &str,
    chat_id: i64,
    text: &str,
    parse_mode: Option<&str>,
) -> Result<i64> {
    send_message_with_markup(http, api_base, token, chat_id, text, parse_mode, None).await
}

pub async fn send_message_with_markup(
    http: &reqwest::Client,
    api_base: &str,
    token: &str,
    chat_id: i64,
    text: &str,
    parse_mode: Option<&str>,
    reply_markup: Option<InlineKeyboardMarkup>,
) -> Result<i64> {
    let url = format!("{api_base}/bot{token}/sendMessage");
    let body = SendMessageReq {
        chat_id,
        text: text.to_string(),
        parse_mode: parse_mode.map(String::from),
        reply_markup,
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

pub async fn answer_callback_query(
    http: &reqwest::Client,
    api_base: &str,
    token: &str,
    callback_query_id: &str,
) -> Result<()> {
    let url = format!("{api_base}/bot{token}/answerCallbackQuery");
    let body = serde_json::json!({ "callback_query_id": callback_query_id });
    let _ = http
        .post(&url)
        .timeout(Duration::from_secs(10))
        .json(&body)
        .send()
        .await;
    Ok(())
}

pub async fn edit_message_text(
    http: &reqwest::Client,
    api_base: &str,
    token: &str,
    chat_id: i64,
    message_id: i64,
    text: &str,
    parse_mode: Option<&str>,
) -> Result<()> {
    let url = format!("{api_base}/bot{token}/editMessageText");
    let body = EditMessageTextReq {
        chat_id,
        message_id,
        text: text.to_string(),
        parse_mode: parse_mode.map(String::from),
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
            parsed.description.unwrap_or_else(|| "unknown error".into()),
        ));
    }
    Ok(parsed.result.map(|r| r.message_id).unwrap_or(0))
}

/// Escape text for safe inclusion in HTML parse_mode messages.
pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Convert standard Markdown to Telegram MarkdownV2.
pub fn md_to_telegram_mdv2(md: &str) -> String {
    telegramify_markdown::convert(md)
}
