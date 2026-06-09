use serde::{Deserialize, Serialize};

// ─── tenant_access_token ──────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct TenantTokenReq {
    pub app_id: String,
    pub app_secret: String,
}

#[derive(Debug, Deserialize)]
pub struct TenantTokenResp {
    pub code: i32,
    pub msg: String,
    #[serde(default)]
    pub tenant_access_token: String,
    #[serde(default)]
    pub expire: i64,
}

// ─── event callback (v2.0 schema) ─────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct EventCallback {
    #[serde(default)]
    pub schema: String,
    pub header: Option<EventHeader>,
    pub event: Option<EventBody>,
    /// URL verification fields (present only during challenge).
    #[serde(default)]
    pub challenge: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default, rename = "type")]
    pub event_type_field: Option<String>,
}

impl EventCallback {
    pub fn is_url_verification(&self) -> bool {
        self.event_type_field.as_deref() == Some("url_verification")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventHeader {
    pub event_id: String,
    pub event_type: String,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub create_time: String,
    #[serde(default)]
    pub tenant_key: String,
    #[serde(default)]
    pub app_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventBody {
    pub sender: Option<EventSender>,
    pub message: Option<EventMessage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventSender {
    pub sender_id: Option<SenderId>,
    #[serde(default)]
    pub sender_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SenderId {
    #[serde(default)]
    pub open_id: String,
    #[serde(default)]
    pub user_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventMessage {
    #[serde(default)]
    pub message_id: String,
    #[serde(default)]
    pub chat_id: String,
    #[serde(default)]
    pub chat_type: String,
    #[serde(default)]
    pub message_type: String,
    #[serde(default)]
    pub content: String,
}

// ─── message content subtypes ─────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct TextContent {
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImageContent {
    #[serde(default)]
    pub image_key: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileContent {
    #[serde(default)]
    pub file_key: String,
    #[serde(default)]
    pub file_name: String,
}

// ─── outbound: send message ───────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SendMessageReq<'a> {
    pub receive_id: &'a str,
    pub msg_type: &'a str,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct SendMessageResp {
    pub code: i32,
    #[serde(default)]
    pub msg: String,
    pub data: Option<SendMessageData>,
}

#[derive(Debug, Deserialize)]
pub struct SendMessageData {
    #[serde(default)]
    pub message_id: String,
}

// ─── outbound: reply message ──────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ReplyMessageReq {
    pub msg_type: String,
    pub content: String,
}

// ─── upload image ─────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct UploadImageResp {
    pub code: i32,
    #[serde(default)]
    pub msg: String,
    pub data: Option<UploadImageData>,
}

#[derive(Debug, Deserialize)]
pub struct UploadImageData {
    #[serde(default)]
    pub image_key: String,
}

// ─── interactive card builder ─────────────────────────────────

/// Build a streaming card JSON string with markdown content and a status note.
///
/// `status` is one of "processing" / "finished" / "failed".
pub fn build_streaming_card(content: &str, status: &str) -> String {
    let (template, note) = match status {
        "finished" => ("green", "✅ 完成"),
        "failed" => ("red", "❌ 出错"),
        _ => ("blue", "⏳ 处理中..."),
    };
    let card = serde_json::json!({
        "config": {"wide_screen_mode": true},
        "header": {
            "title": {"tag": "plain_text", "content": "🤖 Agent"},
            "template": template,
        },
        "elements": [
            {"tag": "markdown", "content": content},
            {"tag": "note", "elements": [
                {"tag": "plain_text", "content": note}
            ]}
        ]
    });
    card.to_string()
}

// ─── challenge response ───────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ChallengeResp {
    pub challenge: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_url_verification() {
        let raw = r#"{
            "challenge": "ajls384kdjx98XX",
            "token": "test_token",
            "type": "url_verification"
        }"#;
        let cb: EventCallback = serde_json::from_str(raw).unwrap();
        assert!(cb.is_url_verification());
        assert_eq!(cb.challenge.unwrap(), "ajls384kdjx98XX");
        assert_eq!(cb.token.unwrap(), "test_token");
    }

    #[test]
    fn deserialize_message_event() {
        let raw = r#"{
            "schema": "2.0",
            "header": {
                "event_id": "ev_001",
                "event_type": "im.message.receive_v1",
                "token": "vtoken",
                "create_time": "1700000000000",
                "tenant_key": "tk",
                "app_id": "cli_xxx"
            },
            "event": {
                "sender": {
                    "sender_id": { "open_id": "ou_xxx", "user_id": "uid" },
                    "sender_type": "user"
                },
                "message": {
                    "message_id": "om_xxx",
                    "chat_id": "oc_xxx",
                    "chat_type": "p2p",
                    "message_type": "text",
                    "content": "{\"text\":\"hello\"}"
                }
            }
        }"#;
        let cb: EventCallback = serde_json::from_str(raw).unwrap();
        assert!(!cb.is_url_verification());
        let header = cb.header.unwrap();
        assert_eq!(header.event_type, "im.message.receive_v1");
        assert_eq!(header.event_id, "ev_001");
        let event = cb.event.unwrap();
        let sender = event.sender.unwrap();
        assert_eq!(sender.sender_id.unwrap().open_id, "ou_xxx");
        let msg = event.message.unwrap();
        assert_eq!(msg.message_type, "text");
        assert_eq!(msg.chat_type, "p2p");
        let content: TextContent = serde_json::from_str(&msg.content).unwrap();
        assert_eq!(content.text, "hello");
    }

    #[test]
    fn deserialize_token_resp() {
        let raw = r#"{
            "code": 0,
            "msg": "ok",
            "tenant_access_token": "t-abc123",
            "expire": 7200
        }"#;
        let resp: TenantTokenResp = serde_json::from_str(raw).unwrap();
        assert_eq!(resp.code, 0);
        assert_eq!(resp.tenant_access_token, "t-abc123");
        assert_eq!(resp.expire, 7200);
    }

    #[test]
    fn serialize_send_message() {
        let req = SendMessageReq {
            receive_id: "ou_xxx",
            msg_type: "text",
            content: r#"{"text":"hi"}"#.to_string(),
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"receive_id\":\"ou_xxx\""));
        assert!(s.contains("\"msg_type\":\"text\""));
    }

    #[test]
    fn build_streaming_card_processing() {
        let json = build_streaming_card("hello **world**", "processing");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["header"]["template"], "blue");
        assert_eq!(v["elements"][0]["content"], "hello **world**");
        assert_eq!(v["elements"][1]["elements"][0]["content"], "⏳ 处理中...");
    }

    #[test]
    fn build_streaming_card_finished() {
        let json = build_streaming_card("done", "finished");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["header"]["template"], "green");
        assert_eq!(v["elements"][1]["elements"][0]["content"], "✅ 完成");
    }

    #[test]
    fn build_streaming_card_failed() {
        let json = build_streaming_card("err", "failed");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["header"]["template"], "red");
        assert_eq!(v["elements"][1]["elements"][0]["content"], "❌ 出错");
    }
}
