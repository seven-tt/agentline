use agentline_bridge::types::{
    Attachment, AttachmentKind, Command, InboundMessage, InboundPayload, MessageKind, UserContent,
};

use crate::commands::parse_text;

/// Default inbound parser for IM sources: text → command or content,
/// media → attachments. Call this from your `InputSource::parse_message` impl.
pub fn default_parse_message(msg: &InboundMessage) -> InboundPayload {
    match &msg.kind {
        MessageKind::Text { text } => {
            let cmd = parse_text(text);
            match cmd {
                Command::Plain(t) => InboundPayload::Content(UserContent::text(t)),
                other => InboundPayload::Command(other),
            }
        }
        MessageKind::Image {
            local_path,
            caption,
        } => {
            let mut content = UserContent {
                text: caption.clone(),
                attachments: Vec::new(),
            };
            if let Some(path) = local_path {
                content.attachments.push(Attachment {
                    kind: AttachmentKind::Image,
                    local_path: path.clone(),
                    name: None,
                    transcript: None,
                });
            }
            InboundPayload::Content(content)
        }
        MessageKind::Voice {
            transcript,
            local_path,
        } => {
            let mut content = UserContent {
                text: transcript.clone(),
                attachments: Vec::new(),
            };
            if let Some(path) = local_path {
                content.attachments.push(Attachment {
                    kind: AttachmentKind::Voice,
                    local_path: path.clone(),
                    name: None,
                    transcript: transcript.clone(),
                });
            }
            InboundPayload::Content(content)
        }
        MessageKind::File { local_path, name } => InboundPayload::Content(UserContent {
            text: None,
            attachments: vec![Attachment {
                kind: AttachmentKind::File,
                local_path: local_path.clone(),
                name: Some(name.clone()),
                transcript: None,
            }],
        }),
        MessageKind::Video {
            local_path,
            caption,
        } => {
            let mut content = UserContent {
                text: caption.clone(),
                attachments: Vec::new(),
            };
            if let Some(path) = local_path {
                content.attachments.push(Attachment {
                    kind: AttachmentKind::Video,
                    local_path: path.clone(),
                    name: None,
                    transcript: None,
                });
            }
            InboundPayload::Content(content)
        }
    }
}
