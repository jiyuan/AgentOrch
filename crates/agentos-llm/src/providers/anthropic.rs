use crate::providers::content::{
    append_descriptors, document_mime, format_text_document, image_mime, read_base64,
    read_text_document,
};
use crate::providers::post_json;
use agentos_proto::{Attachment, AttachmentKind, Message, MessageRole};
use serde_json::{json, Value};
use std::env;

pub async fn complete(model: &str, messages: &[Message]) -> Result<String, String> {
    let api_key =
        env::var("ANTHROPIC_API_KEY").map_err(|_| "missing ANTHROPIC_API_KEY".to_owned())?;
    let base_url = env::var("AGENTOS_ANTHROPIC_BASE_URL")
        .or_else(|_| env::var("ANTHROPIC_BASE_URL"))
        .unwrap_or_else(|_| "https://api.anthropic.com/v1".to_owned());

    let serialized = messages
        .iter()
        .filter(|message| message.role != MessageRole::System)
        .map(build_message)
        .collect::<Vec<_>>();

    let payload = json!({
        "model": model,
        "max_tokens": 1024,
        "messages": serialized,
    });
    let response = post_json(
        "llm",
        &format!("{}/messages", base_url.trim_end_matches('/')),
        &[
            ("x-api-key", api_key),
            ("anthropic-version", "2023-06-01".to_owned()),
            ("Content-Type", "application/json".to_owned()),
        ],
        &payload,
    )
    .await?;
    response
        .body
        .get("content")
        .and_then(Value::as_array)
        .and_then(|content| content.first())
        .and_then(|part| part.get("text"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            format!(
                "Anthropic response missing assistant content: {}",
                response.body
            )
        })
}

fn build_message(message: &Message) -> Value {
    let role = match message.role {
        MessageRole::Assistant => "assistant",
        MessageRole::Tool | MessageRole::User => "user",
        MessageRole::System => "user",
    };

    let base_text = if message.role == MessageRole::Tool {
        format!("Tool result: {}", message.content)
    } else {
        message.content.to_string()
    };

    if message.attachments.is_empty() {
        return json!({
            "role": role,
            "content": base_text,
        });
    }

    // Split each attachment into either an inline content block or a textual
    // descriptor that gets appended to the leading text block.
    let mut inline_blocks: Vec<Value> = Vec::new();
    let mut fallback_attachments: Vec<&Attachment> = Vec::new();
    for attachment in &message.attachments {
        match content_block_for(attachment) {
            Some(block) => inline_blocks.push(block),
            None => fallback_attachments.push(attachment),
        }
    }

    let leading_text = if fallback_attachments.is_empty() {
        base_text
    } else {
        let owned: Vec<Attachment> = fallback_attachments.into_iter().cloned().collect();
        append_descriptors(&base_text, &owned)
    };

    // Always lead with a text block. Some models reply with a generic
    // "I don't have access to that file" when given an image content block
    // alone; a neutral placeholder keeps the turn well-formed.
    let text_part = if leading_text.is_empty() {
        "(user attached files without a caption)".to_owned()
    } else {
        leading_text
    };
    let mut blocks: Vec<Value> = Vec::with_capacity(inline_blocks.len() + 1);
    blocks.push(json!({ "type": "text", "text": text_part }));
    blocks.extend(inline_blocks);

    json!({
        "role": role,
        "content": blocks,
    })
}

fn content_block_for(attachment: &Attachment) -> Option<Value> {
    match attachment.kind {
        AttachmentKind::Image => image_block(attachment),
        AttachmentKind::Document => document_block(attachment),
    }
}

fn image_block(attachment: &Attachment) -> Option<Value> {
    let media_type = image_mime(attachment)?;
    let data = read_base64(&attachment.path).ok()?;
    Some(json!({
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": media_type,
            "data": data,
        }
    }))
}

fn document_block(attachment: &Attachment) -> Option<Value> {
    if let Some(media_type) = document_mime(attachment) {
        if let Ok(data) = read_base64(&attachment.path) {
            return Some(json!({
                "type": "document",
                "source": {
                    "type": "base64",
                    "media_type": media_type,
                    "data": data,
                }
            }));
        }
    }
    if let Some(Ok(body)) = read_text_document(attachment) {
        let formatted = format_text_document(&attachment.name, &body);
        return Some(json!({
            "type": "text",
            "text": formatted,
        }));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_proto::{Attachment, AttachmentKind};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn write_tmp(name: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("anthropic-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    fn image_message(path: PathBuf, mime: Option<&str>, text: &str) -> Message {
        Message {
            role: MessageRole::User,
            content: Arc::from(text),
            attachments: vec![Attachment {
                kind: AttachmentKind::Image,
                name: Arc::from("photo.jpg"),
                path,
                mime: mime.map(Arc::from),
                size: Some(3),
                source: None,
            }],
            metadata: Default::default(),
        }
    }

    #[test]
    fn text_only_message_keeps_string_content() {
        let msg = Message::text(MessageRole::User, "hi");
        let value = build_message(&msg);
        assert_eq!(value.get("content").and_then(Value::as_str), Some("hi"));
    }

    #[test]
    fn image_attachment_emits_image_block() {
        let path = write_tmp("photo.jpg", &[0xff, 0xd8, 0xff]);
        let msg = image_message(path, Some("image/jpeg"), "look");
        let value = build_message(&msg);
        let blocks = value.get("content").and_then(Value::as_array).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "look");
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["source"]["media_type"], "image/jpeg");
        assert!(blocks[1]["source"]["data"].as_str().unwrap().len() > 0);
    }

    #[test]
    fn unsupported_image_falls_back_to_descriptor() {
        let path = write_tmp("photo.bmp", &[0x42, 0x4d]);
        let msg = image_message(path, Some("image/bmp"), "hi");
        let value = build_message(&msg);
        let blocks = value.get("content").and_then(Value::as_array).unwrap();
        // No inline image block; only a single text block carrying the descriptor.
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "text");
        let text = blocks[0]["text"].as_str().unwrap();
        assert!(text.starts_with("hi"));
        assert!(text.contains("[attached image:"));
    }

    #[test]
    fn pdf_attachment_emits_document_block() {
        let path = write_tmp("spec.pdf", b"%PDF-1.4 fake");
        let msg = Message {
            role: MessageRole::User,
            content: Arc::from(""),
            attachments: vec![Attachment {
                kind: AttachmentKind::Document,
                name: Arc::from("spec.pdf"),
                path,
                mime: Some(Arc::from("application/pdf")),
                size: Some(13),
                source: None,
            }],
            metadata: Default::default(),
        };
        let value = build_message(&msg);
        let blocks = value.get("content").and_then(Value::as_array).unwrap();
        // Leading placeholder text block + document block.
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "document");
        assert_eq!(blocks[1]["source"]["media_type"], "application/pdf");
    }

    #[test]
    fn missing_file_falls_back_to_descriptor() {
        let msg = image_message(
            PathBuf::from("/nonexistent/missing.jpg"),
            Some("image/jpeg"),
            "hi",
        );
        let value = build_message(&msg);
        let blocks = value.get("content").and_then(Value::as_array).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "text");
    }
}
