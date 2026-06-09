//! Wire-format types for DingTalk Stream API.
//!
//! Reference: https://github.com/open-dingtalk/dingtalk-stream-sdk-go
//! Stream gateway open: `POST https://api.dingtalk.com/v1.0/gateway/connections/open`

use serde::{Deserialize, Serialize};

// ─── connection-open handshake ─────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ConnectionOpenReq {
    /// Self-built app: appKey. Third-party app: suiteKey.
    #[serde(rename = "clientId")]
    pub client_id: String,
    /// Self-built app: appSecret. Third-party app: suiteSecret.
    #[serde(rename = "clientSecret")]
    pub client_secret: String,
    pub subscriptions: Vec<Subscription>,
    pub ua: String,
    #[serde(rename = "localIp")]
    pub local_ip: String,
    #[serde(default)]
    pub extras: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    /// "SYSTEM" / "EVENT" / "CALLBACK"
    #[serde(rename = "type")]
    pub kind: String,
    pub topic: String,
}

#[derive(Debug, Deserialize)]
pub struct ConnectionOpenResp {
    /// "wss://stream.dingtalk.com/connect" or similar.
    pub endpoint: String,
    pub ticket: String,
}

// ─── stream data frames ────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DataFrame {
    #[serde(default, rename = "specVersion")]
    pub spec_version: String,
    /// "SYSTEM" / "EVENT" / "CALLBACK"
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub time: i64,
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
    /// Inner JSON encoded as a string. Caller decodes per topic.
    #[serde(default)]
    pub data: String,
}

impl DataFrame {
    pub fn topic(&self) -> &str {
        self.headers.get("topic").map(String::as_str).unwrap_or("")
    }
    pub fn message_id(&self) -> &str {
        self.headers
            .get("messageId")
            .map(String::as_str)
            .unwrap_or("")
    }
}

#[derive(Debug, Serialize)]
pub struct DataFrameResponse {
    pub code: i32,
    pub headers: std::collections::HashMap<String, String>,
    pub message: String,
    pub data: String,
}

impl DataFrameResponse {
    pub fn ok(message_id: &str) -> Self {
        let mut headers = std::collections::HashMap::new();
        headers.insert("messageId".into(), message_id.to_string());
        headers.insert("contentType".into(), "application/json".into());
        Self {
            code: 200,
            headers,
            message: "ok".into(),
            data: String::new(),
        }
    }
}

// ─── bot callback payload (the `data` of CALLBACK frames) ───────

/// Decoded payload of a `/v1.0/im/bot/messages/get` callback.
///
/// Mirrors DingTalk's `BotCallbackDataModel` — only the fields we need.
#[derive(Debug, Clone, Deserialize)]
pub struct BotCallback {
    #[serde(default, rename = "msgId")]
    pub msg_id: String,
    #[serde(default, rename = "senderStaffId")]
    pub sender_staff_id: String,
    #[serde(default, rename = "senderNick")]
    pub sender_nick: String,
    /// "1" = p2p, "2" = group
    #[serde(default, rename = "conversationType")]
    pub conversation_type: String,
    #[serde(default, rename = "conversationId")]
    pub conversation_id: String,
    #[serde(default, rename = "conversationTitle")]
    pub conversation_title: String,
    /// Per-message webhook URL. Use it once to reply, expires after a few minutes.
    #[serde(default, rename = "sessionWebhook")]
    pub session_webhook: String,
    #[serde(default, rename = "sessionWebhookExpiredTime")]
    pub session_webhook_expired_time: Option<i64>,
    /// "text" / "richText" / "image" / "audio" / "picture"
    #[serde(default)]
    pub msgtype: String,
    /// Present for text messages. The interesting fields elsewhere are dynamic.
    #[serde(default)]
    pub text: Option<TextContent>,
    #[serde(default, rename = "createAt")]
    pub create_at: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TextContent {
    #[serde(default)]
    pub content: String,
}

impl BotCallback {
    pub fn is_group(&self) -> bool {
        self.conversation_type == "2"
    }
}

// ─── outbound: reply via session webhook ───────────────────────
//
// Schema: https://open.dingtalk.com/document/orgapp/the-robot-sends-a-group-message
// We use the "text" msgtype which the simplest possible.
//
// POST <session_webhook>
// { "msgtype": "text", "text": { "content": "..." } }

#[derive(Debug, Serialize)]
pub struct SessionWebhookText<'a> {
    pub msgtype: &'static str,
    pub text: SessionWebhookTextBody<'a>,
}

#[derive(Debug, Serialize)]
pub struct SessionWebhookTextBody<'a> {
    pub content: &'a str,
}

impl<'a> SessionWebhookText<'a> {
    pub fn new(content: &'a str) -> Self {
        Self {
            msgtype: "text",
            text: SessionWebhookTextBody { content },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_data_frame() {
        let raw = r#"{
            "specVersion": "1.0",
            "type": "CALLBACK",
            "time": 1700000000,
            "headers": {
                "topic": "/v1.0/im/bot/messages/get",
                "messageId": "msg-1"
            },
            "data": "{\"msgId\":\"m1\",\"senderStaffId\":\"u1\"}"
        }"#;
        let df: DataFrame = serde_json::from_str(raw).unwrap();
        assert_eq!(df.kind, "CALLBACK");
        assert_eq!(df.topic(), "/v1.0/im/bot/messages/get");
        assert_eq!(df.message_id(), "msg-1");
        let cb: BotCallback = serde_json::from_str(&df.data).unwrap();
        assert_eq!(cb.msg_id, "m1");
        assert_eq!(cb.sender_staff_id, "u1");
    }

    #[test]
    fn deserialize_text_callback() {
        let raw = r#"{
            "msgId": "msg-1",
            "senderStaffId": "u1",
            "senderNick": "Alice",
            "conversationType": "1",
            "conversationId": "cid",
            "sessionWebhook": "https://oapi.dingtalk.com/robot/sendBySession?session=xxx",
            "msgtype": "text",
            "text": { "content": "你好" },
            "createAt": 1700000000000
        }"#;
        let cb: BotCallback = serde_json::from_str(raw).unwrap();
        assert!(!cb.is_group());
        assert_eq!(cb.text.unwrap().content, "你好");
        assert!(cb.session_webhook.starts_with("https://"));
    }

    #[test]
    fn ack_response_shape() {
        let resp = DataFrameResponse::ok("msg-1");
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("\"code\":200"));
        assert!(s.contains("\"messageId\":\"msg-1\""));
    }
}
