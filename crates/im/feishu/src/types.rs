use serde::{Deserialize, Deserializer, Serialize};

fn nullable_string<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    Option::<String>::deserialize(d).map(|o| o.unwrap_or_default())
}

// ─── tenant_access_token ──────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct TenantTokenReq {
    pub app_id: String,
    pub app_secret: String,
}

#[derive(Debug, Deserialize)]
pub struct TenantTokenResp {
    pub code: i32,
    #[serde(default, deserialize_with = "nullable_string")]
    pub msg: String,
    #[serde(default, deserialize_with = "nullable_string")]
    pub tenant_access_token: String,
    #[serde(default)]
    pub expire: i64,
}

// ─── event callback (v2.0 schema) ─────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct EventCallback {
    #[serde(default, deserialize_with = "nullable_string")]
    pub schema: String,
    pub header: Option<EventHeader>,
    pub event: Option<EventBody>,
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
    #[serde(default, deserialize_with = "nullable_string")]
    pub event_id: String,
    #[serde(default, deserialize_with = "nullable_string")]
    pub event_type: String,
    #[serde(default, deserialize_with = "nullable_string")]
    pub token: String,
    #[serde(default, deserialize_with = "nullable_string")]
    pub create_time: String,
    #[serde(default, deserialize_with = "nullable_string")]
    pub tenant_key: String,
    #[serde(default, deserialize_with = "nullable_string")]
    pub app_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventBody {
    pub sender: Option<EventSender>,
    pub message: Option<EventMessage>,
    pub operator: Option<CardOperator>,
    pub action: Option<CardAction>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CardOperator {
    #[serde(default, deserialize_with = "nullable_string")]
    pub open_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CardAction {
    pub value: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventSender {
    pub sender_id: Option<SenderId>,
    #[serde(default, deserialize_with = "nullable_string")]
    pub sender_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SenderId {
    #[serde(default, deserialize_with = "nullable_string")]
    pub open_id: String,
    #[serde(default, deserialize_with = "nullable_string")]
    pub user_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventMessage {
    #[serde(default, deserialize_with = "nullable_string")]
    pub message_id: String,
    #[serde(default, deserialize_with = "nullable_string")]
    pub chat_id: String,
    #[serde(default, deserialize_with = "nullable_string")]
    pub chat_type: String,
    #[serde(default, deserialize_with = "nullable_string")]
    pub message_type: String,
    #[serde(default, deserialize_with = "nullable_string")]
    pub content: String,
}

// ─── message content subtypes ─────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct TextContent {
    #[serde(default, deserialize_with = "nullable_string")]
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImageContent {
    #[serde(default, deserialize_with = "nullable_string")]
    pub image_key: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileContent {
    #[serde(default, deserialize_with = "nullable_string")]
    pub file_key: String,
    #[serde(default, deserialize_with = "nullable_string")]
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
    #[serde(default, deserialize_with = "nullable_string")]
    pub msg: String,
    pub data: Option<SendMessageData>,
}

#[derive(Debug, Deserialize)]
pub struct SendMessageData {
    #[serde(default, deserialize_with = "nullable_string")]
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
    #[serde(default, deserialize_with = "nullable_string")]
    pub msg: String,
    pub data: Option<UploadImageData>,
}

#[derive(Debug, Deserialize)]
pub struct UploadImageData {
    #[serde(default, deserialize_with = "nullable_string")]
    pub image_key: String,
}

// ─── interactive card builder ─────────────────────────────────

pub fn build_streaming_card(content: &str, status: &str) -> String {
    use rust_i18n::t;
    let (template, note) = match status {
        "finished" => ("green", t!("im.stream_done").to_string()),
        "failed" => ("red", t!("im.stream_failed").to_string()),
        "thinking" => ("turquoise", "💭 ...".to_string()),
        _ => ("blue", t!("im.stream_processing").to_string()),
    };
    let converted = to_feishu_markdown(content);
    let card = serde_json::json!({
        "config": {"wide_screen_mode": true},
        "header": {
            "title": {"tag": "plain_text", "content": "🤖 Agent"},
            "template": template,
        },
        "elements": [
            {"tag": "markdown", "content": converted},
            {"tag": "note", "elements": [
                {"tag": "plain_text", "content": note}
            ]}
        ]
    });
    card.to_string()
}

/// Convert standard Markdown to Feishu card-compatible Markdown.
///
/// Feishu card markdown does NOT support: `#` headers, `>` blockquotes,
/// or tables. This function converts them to supported equivalents.
pub fn to_feishu_markdown(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut in_code_block = false;
    let mut table_buf: Vec<Vec<String>> = Vec::new();

    let lines: Vec<&str> = src.split('\n').collect();
    for line in &lines {
        if line.trim_start().starts_with("```") {
            flush_table(&mut out, &mut table_buf);
            if !out.is_empty() {
                out.push('\n');
            }
            in_code_block = !in_code_block;
            out.push_str(line);
            continue;
        }

        if in_code_block {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(line);
            continue;
        }

        let trimmed = line.trim();
        if is_table_row(trimmed) {
            let cells = parse_table_cells(trimmed);
            table_buf.push(cells);
            continue;
        }

        flush_table(&mut out, &mut table_buf);

        if !out.is_empty() {
            out.push('\n');
        }

        // # Header → **Header**
        if let Some(rest) = line
            .strip_prefix("##### ")
            .or_else(|| line.strip_prefix("#### "))
            .or_else(|| line.strip_prefix("### "))
            .or_else(|| line.strip_prefix("## "))
            .or_else(|| line.strip_prefix("# "))
        {
            let rest = rest.trim();
            if !rest.is_empty() {
                out.push_str(&format!("**{rest}**"));
                continue;
            }
        }

        // > blockquote → italic
        if let Some(rest) = line.strip_prefix("> ") {
            out.push_str(&format!("*{rest}*"));
            continue;
        }
        if *line == ">" {
            continue;
        }

        out.push_str(line);
    }
    flush_table(&mut out, &mut table_buf);
    out
}

fn is_table_row(line: &str) -> bool {
    line.starts_with('|') && line.ends_with('|') && line.len() > 1
}

fn is_separator_cell(cell: &str) -> bool {
    !cell.is_empty() && cell.chars().all(|c| c == '-' || c == ':' || c == ' ')
}

fn parse_table_cells(line: &str) -> Vec<String> {
    line[1..line.len() - 1]
        .split('|')
        .map(|c| c.trim().to_string())
        .collect()
}

fn flush_table(out: &mut String, table: &mut Vec<Vec<String>>) {
    if table.is_empty() {
        return;
    }

    // Remove separator rows (|:---|:---|)
    table.retain(|row| !row.iter().all(|c| is_separator_cell(c)));

    if table.is_empty() {
        return;
    }

    let ncols = table[0].len();
    let has_header = table.len() > 1;

    if ncols == 2 && has_header {
        // 2-column key-value: skip header, format as "**Key**  Value"
        for row in &table[1..] {
            if !out.is_empty() {
                out.push('\n');
            }
            let key = row.first().map(|s| s.as_str()).unwrap_or("");
            let val = row.get(1).map(|s| s.as_str()).unwrap_or("");
            out.push_str(&format!("**{key}**  {val}"));
        }
    } else if has_header {
        // Multi-column: header as bold, body as plain
        let hdr = &table[0];
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(
            &hdr.iter()
                .map(|c| format!("**{c}**"))
                .collect::<Vec<_>>()
                .join("  "),
        );
        for row in &table[1..] {
            out.push('\n');
            out.push_str(&row.join("  "));
        }
    } else {
        // No header: just list rows
        for row in table.iter() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&row.join("  "));
        }
    }
    table.clear();
}

pub fn build_card(title: &str, content: &str, template: &str) -> String {
    let converted = to_feishu_markdown(content);
    let card = serde_json::json!({
        "config": {"wide_screen_mode": true},
        "header": {
            "title": {"tag": "plain_text", "content": title},
            "template": template,
        },
        "elements": [
            {"tag": "markdown", "content": converted}
        ]
    });
    card.to_string()
}

/// Build an interactive card for permission requests with approve/deny buttons.
pub fn build_permission_card(kind: &str, what: &str, risk_icon: &str, risk: &str) -> String {
    use rust_i18n::t;
    let template = match risk_icon {
        "🔴" => "red",
        "🟡" => "orange",
        _ => "green",
    };
    let title = t!("im.perm_card_title", kind = kind);
    let converted = to_feishu_markdown(what);
    let approve_once = t!("im.perm_btn_approve_once");
    let approve_session = t!("im.perm_btn_approve_session");
    let deny = t!("im.perm_btn_deny");
    let card = serde_json::json!({
        "config": {"wide_screen_mode": true},
        "header": {
            "title": {"tag": "plain_text", "content": title.to_string()},
            "template": template,
        },
        "elements": [
            {"tag": "markdown", "content": converted},
            {"tag": "action", "actions": [
                {
                    "tag": "button",
                    "text": {"tag": "plain_text", "content": approve_once.to_string()},
                    "type": "primary",
                    "value": {"action": "y"}
                },
                {
                    "tag": "button",
                    "text": {"tag": "plain_text", "content": approve_session.to_string()},
                    "type": "default",
                    "value": {"action": "s"}
                },
                {
                    "tag": "button",
                    "text": {"tag": "plain_text", "content": deny.to_string()},
                    "type": "danger",
                    "value": {"action": "n"}
                }
            ]},
            {"tag": "note", "elements": [
                {"tag": "plain_text", "content": format!("{risk_icon} {risk}")}
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
    fn deserialize_message_event_with_nulls() {
        let raw = r#"{
            "schema": "2.0",
            "header": {
                "event_id": "ev_002",
                "event_type": "im.message.receive_v1",
                "token": null,
                "create_time": null,
                "tenant_key": null,
                "app_id": null
            },
            "event": {
                "sender": {
                    "sender_id": { "open_id": "ou_xxx", "user_id": null },
                    "sender_type": null
                },
                "message": {
                    "message_id": "om_xxx",
                    "chat_id": null,
                    "chat_type": "p2p",
                    "message_type": "text",
                    "content": "{\"text\":\"hello\"}"
                }
            }
        }"#;
        let cb: EventCallback = serde_json::from_str(raw).unwrap();
        let header = cb.header.unwrap();
        assert_eq!(header.token, "");
        assert_eq!(header.create_time, "");
        let event = cb.event.unwrap();
        let sender = event.sender.unwrap();
        assert_eq!(sender.sender_type, "");
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
        let note = rust_i18n::t!("im.stream_processing").to_string();
        assert_eq!(v["elements"][1]["elements"][0]["content"], note);
    }

    #[test]
    fn build_streaming_card_finished() {
        let json = build_streaming_card("done", "finished");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["header"]["template"], "green");
        let note = rust_i18n::t!("im.stream_done").to_string();
        assert_eq!(v["elements"][1]["elements"][0]["content"], note);
    }

    #[test]
    fn build_streaming_card_failed() {
        let json = build_streaming_card("err", "failed");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["header"]["template"], "red");
        let note = rust_i18n::t!("im.stream_failed").to_string();
        assert_eq!(v["elements"][1]["elements"][0]["content"], note);
    }

    #[test]
    fn feishu_markdown_headers() {
        let src = "# Title\n## Sub\n### H3\nplain text";
        let out = to_feishu_markdown(src);
        assert_eq!(out, "**Title**\n**Sub**\n**H3**\nplain text");
    }

    #[test]
    fn feishu_markdown_blockquote() {
        let src = "> quoted line\n>\nnormal";
        let out = to_feishu_markdown(src);
        assert_eq!(out, "*quoted line*\n\nnormal");
    }

    #[test]
    fn feishu_markdown_code_block_preserved() {
        let src = "before\n```rust\n# not a header\n> not a quote\n```\nafter";
        let out = to_feishu_markdown(src);
        assert_eq!(
            out,
            "before\n```rust\n# not a header\n> not a quote\n```\nafter"
        );
    }

    #[test]
    fn feishu_markdown_table_two_col() {
        let src = "| Key | Value |\n| :--- | :--- |\n| Name | Alice |\n| Age | 30 |";
        let out = to_feishu_markdown(src);
        assert_eq!(out, "**Name**  Alice\n**Age**  30");
    }

    #[test]
    fn feishu_markdown_table_multi_col() {
        let src = "| A | B | C |\n|---|---|---|\n| 1 | 2 | 3 |\n| 4 | 5 | 6 |";
        let out = to_feishu_markdown(src);
        assert_eq!(out, "**A**  **B**  **C**\n1  2  3\n4  5  6");
    }

    #[test]
    fn feishu_markdown_table_with_surrounding_text() {
        let src = "## Title\n\n| K | V |\n| --- | --- |\n| foo | bar |\n\nend";
        let out = to_feishu_markdown(src);
        assert_eq!(out, "**Title**\n\n**foo**  bar\n\nend");
    }

    #[test]
    fn feishu_markdown_table_in_code_block() {
        let src = "```\n| not | a | table |\n```";
        let out = to_feishu_markdown(src);
        assert_eq!(out, "```\n| not | a | table |\n```");
    }
}
