use serde::{Deserialize, Serialize};

// ─── getUpdates response ──────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ApiResponse<T> {
    pub ok: bool,
    pub description: Option<String>,
    pub result: Option<T>,
}

#[derive(Debug, Deserialize)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<Message>,
}

#[derive(Debug, Deserialize)]
pub struct Message {
    pub message_id: i64,
    pub from: Option<User>,
    pub chat: Chat,
    pub text: Option<String>,
    pub caption: Option<String>,
    pub photo: Option<Vec<PhotoSize>>,
    pub document: Option<Document>,
    pub voice: Option<Voice>,
    pub video: Option<Video>,
}

#[derive(Debug, Deserialize)]
pub struct User {
    pub id: i64,
    pub first_name: String,
    pub last_name: Option<String>,
    pub username: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
}

#[derive(Debug, Deserialize)]
pub struct PhotoSize {
    pub file_id: String,
    pub file_unique_id: String,
    pub width: i32,
    pub height: i32,
    pub file_size: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct Document {
    pub file_id: String,
    pub file_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Voice {
    pub file_id: String,
    pub duration: i32,
}

#[derive(Debug, Deserialize)]
pub struct Video {
    pub file_id: String,
    pub duration: i32,
}

// ─── sendMessage / editMessageText ────────────────────────────

#[derive(Debug, Serialize)]
pub struct SendMessageReq {
    pub chat_id: i64,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_mode: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct EditMessageTextReq {
    pub chat_id: i64,
    pub message_id: i64,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_mode: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SendMessageResp {
    pub ok: bool,
    pub result: Option<SentMessage>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SentMessage {
    pub message_id: i64,
}

// ─── sendChatAction ───────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SendChatActionReq {
    pub chat_id: i64,
    pub action: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_update_text() {
        let json = r#"{
            "update_id": 123456,
            "message": {
                "message_id": 1,
                "from": {"id": 999, "first_name": "Test", "is_bot": false},
                "chat": {"id": 999, "type": "private"},
                "text": "hello"
            }
        }"#;
        let update: Update = serde_json::from_str(json).unwrap();
        assert_eq!(update.update_id, 123456);
        let msg = update.message.unwrap();
        assert_eq!(msg.text.unwrap(), "hello");
        assert_eq!(msg.chat.id, 999);
        assert_eq!(msg.from.unwrap().id, 999);
    }

    #[test]
    fn deserialize_update_photo() {
        let json = r#"{
            "update_id": 123457,
            "message": {
                "message_id": 2,
                "from": {"id": 999, "first_name": "Test", "is_bot": false},
                "chat": {"id": 999, "type": "private"},
                "photo": [
                    {"file_id": "small", "file_unique_id": "s1", "width": 90, "height": 90},
                    {"file_id": "large", "file_unique_id": "s2", "width": 800, "height": 600, "file_size": 50000}
                ],
                "caption": "a photo"
            }
        }"#;
        let update: Update = serde_json::from_str(json).unwrap();
        let msg = update.message.unwrap();
        assert_eq!(msg.caption.as_deref(), Some("a photo"));
        let photos = msg.photo.unwrap();
        assert_eq!(photos.len(), 2);
        assert_eq!(photos[1].file_id, "large");
    }

    #[test]
    fn serialize_send_message() {
        let req = SendMessageReq {
            chat_id: 123,
            text: "hello".into(),
            parse_mode: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"chat_id\":123"));
        assert!(json.contains("\"text\":\"hello\""));
        assert!(!json.contains("parse_mode"));
    }

    #[test]
    fn serialize_edit_message() {
        let req = EditMessageTextReq {
            chat_id: 123,
            message_id: 456,
            text: "updated".into(),
            parse_mode: Some("MarkdownV2".into()),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"message_id\":456"));
        assert!(json.contains("\"parse_mode\":\"MarkdownV2\""));
    }

    #[test]
    fn deserialize_send_response() {
        let json = r#"{"ok": true, "result": {"message_id": 789}}"#;
        let resp: SendMessageResp = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        assert_eq!(resp.result.unwrap().message_id, 789);
    }
}
