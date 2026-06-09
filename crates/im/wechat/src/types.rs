//! Wire-format types for the Tencent iLink Bot API.
//!
//! Schema reference: openclaw-weixin technical analysis.
//! Base URL: https://ilinkai.weixin.qq.com

use serde::{Deserialize, Serialize};

// ─── login flow ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct GetQrcodeResp {
    pub qrcode: String,
    /// Base64-encoded PNG bytes of the scannable QR image.
    pub qrcode_img_content: String,
    #[serde(default)]
    pub ret: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct QrcodeStatusResp {
    /// "wait" | "scaned" | "confirmed" | "expired" | "scaned_but_redirect" |
    /// "binded_redirect" | "need_verifycode" | "verify_code_blocked"
    pub status: String,
    pub bot_token: Option<String>,
    pub baseurl: Option<String>,
    #[serde(default)]
    pub redirect_host: Option<String>,
    #[serde(default)]
    pub ret: Option<i32>,
}

// ─── long-polling ────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct GetUpdatesReq {
    pub get_updates_buf: String,
    pub base_info: BaseInfo,
}

#[derive(Debug, Serialize)]
pub struct BaseInfo {
    pub channel_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot_agent: Option<String>,
}

impl BaseInfo {
    pub fn current() -> Self {
        Self {
            channel_version: crate::CHANNEL_VERSION.to_string(),
            bot_agent: Some("agentline".to_string()),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct GetUpdatesResp {
    #[serde(default)]
    pub ret: i32,
    #[serde(default)]
    pub msgs: Vec<WeixinMessage>,
    #[serde(default)]
    pub get_updates_buf: Option<String>,
    #[serde(default)]
    pub longpolling_timeout_ms: Option<u64>,
    #[serde(default)]
    pub errmsg: Option<String>,
}

// ─── messages ────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WeixinMessage {
    pub from_user_id: String,
    pub to_user_id: String,
    /// 1 = inbound (user → bot), 2 = outbound (bot → user)
    pub message_type: i32,
    /// 2 = FINISH (complete message)
    pub message_state: i32,
    /// Echo this verbatim on outbound replies — without it the reply lands
    /// in the wrong conversation slot.
    pub context_token: String,
    /// Unique per-message id for delivery/dedup. Required by the server on
    /// outbound sends — without it the message is accepted (ret=0) but never
    /// delivered. Optional on inbound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    #[serde(default)]
    pub item_list: Vec<Item>,
    /// Catch-all for fields we don't model.
    #[serde(flatten, default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Item {
    /// 1=Text 2=Image 3=Voice 4=File 5=Video 11=ToolCallStart 12=ToolCallResult 13=Thinking 14=StreamSignal
    #[serde(rename = "type")]
    pub item_type: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_item: Option<TextItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_item: Option<ImageItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_item: Option<VoiceItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_item: Option<FileItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_item: Option<VideoItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_start_item: Option<ToolCallStartItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_result_item: Option<ToolCallResultItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_item: Option<ThinkingItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_signal_item: Option<StreamSignalItem>,
}

impl Item {
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            item_type: 1,
            text_item: Some(TextItem { text: s.into() }),
            image_item: None,
            voice_item: None,
            file_item: None,
            video_item: None,
            tool_call_start_item: None,
            tool_call_result_item: None,
            thinking_item: None,
            stream_signal_item: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TextItem {
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ImageItem {
    /// Hex-encoded AES key (preferred by newer iLink servers).
    #[serde(default)]
    pub aeskey: Option<String>,
    #[serde(default)]
    pub media: Option<MediaRef>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VoiceItem {
    /// Server-side auto transcription if present.
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub media: Option<MediaRef>,
    #[serde(flatten, default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FileItem {
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub media: Option<MediaRef>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VideoItem {
    #[serde(default)]
    pub media: Option<MediaRef>,
}

/// CDN reference + decryption key for a media object.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MediaRef {
    pub encrypt_query_param: String,
    /// Base64-encoded AES-128 key.
    #[serde(default)]
    pub aes_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolCallStartItem {
    pub tool_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args_json: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolCallResultItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ThinkingItem {
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StreamSignalItem {
    pub stream_type: String,
    pub stream_id: String,
    pub ilink_stream_ticket: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    pub action: String,
}

// ─── outbound ────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SendMessageReq {
    pub msg: WeixinMessage,
    pub base_info: BaseInfo,
}

#[derive(Debug, Deserialize)]
pub struct SendMessageResp {
    #[serde(default)]
    pub ret: i32,
    #[serde(default)]
    pub errmsg: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SendTypingReq {
    pub to_user_id: String,
    pub typing_ticket: String,
    pub context_token: String,
}

#[derive(Debug, Serialize)]
pub struct GetConfigReq {
    pub to_user_id: String,
    pub context_token: String,
}

#[derive(Debug, Deserialize)]
pub struct GetConfigResp {
    #[serde(default)]
    pub ret: i32,
    #[serde(default)]
    pub typing_ticket: Option<String>,
}

// ─── piece streaming ─────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct InitStreamReq {
    pub device_id: String,
    pub client_stream_id: String,
    pub business_type: i32,
}

#[derive(Debug, Deserialize)]
pub struct InitStreamResp {
    #[serde(flatten, default)]
    pub base_response: Option<BaseResponse>,
    #[serde(default)]
    pub stream_ticket: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct BaseResponse {
    #[serde(default)]
    pub ret: i32,
    #[serde(default)]
    pub errmsg: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct PieceItem {
    pub piece_seq: i32,
    pub piece_data: String,
}

#[derive(Debug, Serialize)]
pub struct SyncStreamReq {
    pub device_id: String,
    pub client_stream_id: String,
    pub business_type: i32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub up_piece_list: Vec<PieceItem>,
    #[serde(default)]
    pub end_up_piece_seq: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abort_info: Option<AbortInfo>,
}

#[derive(Debug, Serialize)]
pub struct AbortInfo {
    pub abort_type: i32,
    #[serde(default)]
    pub abort_detail_error_code: i32,
    #[serde(default)]
    pub abort_detail_error_msg: String,
}

#[derive(Debug, Deserialize)]
pub struct SyncStreamResp {
    #[serde(flatten, default)]
    pub base_response: Option<BaseResponse>,
    #[serde(default)]
    pub abort_info: Option<AbortInfoResp>,
}

#[derive(Debug, Deserialize)]
pub struct AbortInfoResp {
    #[serde(default)]
    pub abort_type: i32,
    #[serde(default)]
    pub abort_detail_error_code: i32,
    #[serde(default)]
    pub abort_detail_error_msg: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_inbound_text_msg() {
        // Synthesized to match the schema in the openclaw-weixin technical analysis.
        let raw = r#"{
            "ret": 0,
            "msgs": [{
                "from_user_id": "o9cq800kum_xxx@im.wechat",
                "to_user_id": "e06c1ceea05e@im.bot",
                "message_type": 1,
                "message_state": 2,
                "context_token": "AARzJWAFAAABAAAA",
                "item_list": [{
                    "type": 1,
                    "text_item": { "text": "你好" }
                }]
            }],
            "get_updates_buf": "cursor1",
            "longpolling_timeout_ms": 35000
        }"#;
        let resp: GetUpdatesResp = serde_json::from_str(raw).unwrap();
        assert_eq!(resp.ret, 0);
        assert_eq!(resp.msgs.len(), 1);
        let m = &resp.msgs[0];
        assert_eq!(m.message_type, 1);
        assert_eq!(m.context_token, "AARzJWAFAAABAAAA");
        assert_eq!(m.item_list.len(), 1);
        assert_eq!(m.item_list[0].item_type, 1);
        assert_eq!(m.item_list[0].text_item.as_ref().unwrap().text, "你好");
        assert_eq!(resp.get_updates_buf.as_deref(), Some("cursor1"));
    }

    #[test]
    fn build_send_request() {
        let req = SendMessageReq {
            msg: WeixinMessage {
                from_user_id: String::new(),
                to_user_id: "o9cq800kum_xxx@im.wechat".into(),
                message_type: 2,
                message_state: 2,
                context_token: "ctx".into(),
                client_id: Some("agentline:1-deadbeef".into()),
                group_id: None,
                item_list: vec![Item::text("回复你")],
                extra: Default::default(),
            },
            base_info: BaseInfo::current(),
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"context_token\":\"ctx\""));
        assert!(s.contains("\"text\":\"回复你\""));
        assert!(s.contains("\"type\":1"));
        assert!(s.contains("\"message_type\":2"));
    }

    #[test]
    fn voice_with_transcript() {
        let raw = r#"{
            "type": 3,
            "voice_item": { "text": "转写后的文字" }
        }"#;
        let item: Item = serde_json::from_str(raw).unwrap();
        assert_eq!(item.item_type, 3);
        assert_eq!(
            item.voice_item.as_ref().unwrap().text.as_deref(),
            Some("转写后的文字")
        );
    }

    #[test]
    fn empty_msgs_default() {
        let raw = r#"{ "ret": 0, "get_updates_buf": "buf" }"#;
        let resp: GetUpdatesResp = serde_json::from_str(raw).unwrap();
        assert!(resp.msgs.is_empty());
    }
}
