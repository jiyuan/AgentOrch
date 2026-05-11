use crate::providers::content::{
    append_descriptors, document_mime, format_text_document, image_mime, read_base64,
    read_text_document,
};
use crate::providers::{first_env, format_openai_error, post_json};
use agentos_proto::{Attachment, AttachmentKind, Message, MessageRole};
use serde_json::{json, Value};
use std::env;

pub async fn complete(model: &str, messages: &[Message]) -> Result<String, String> {
    let api_key = env::var("OPENAI_API_KEY").map_err(|_| "missing OPENAI_API_KEY".to_owned())?;
    let base_url = env::var("AGENTOS_OPENAI_BASE_URL")
        .or_else(|_| env::var("OPENAI_BASE_URL"))
        .unwrap_or_else(|_| "https://api.openai.com/v1".to_owned());
    let mut headers = vec![
        ("Authorization", format!("Bearer {api_key}")),
        ("Content-Type", "application/json".to_owned()),
    ];
    if let Some(organization) = first_env(["OPENAI_ORGANIZATION", "OPENAI_ORG_ID"]) {
        headers.push(("OpenAI-Organization", organization));
    }
    if let Some(project) = first_env(["OPENAI_PROJECT", "OPENAI_PROJECT_ID"]) {
        headers.push(("OpenAI-Project", project));
    }
    let serialized = messages.iter().map(build_message).collect::<Vec<_>>();
    let payload = json!({
        "model": model,
        "messages": serialized,
        "temperature": 0.7,
    });
    let response = post_json(
        "llm",
        &format!("{}/chat/completions", base_url.trim_end_matches('/')),
        &headers,
        &payload,
    )
    .await?;
    if let Some(error) = response.body.get("error") {
        return Err(format_openai_error(&response, error));
    }
    response
        .body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            format!(
                "OpenAI response missing assistant content: {}",
                response.body
            )
        })
}

fn build_message(message: &Message) -> Value {
    let role = match message.role {
        MessageRole::Assistant => "assistant",
        MessageRole::System => "system",
        MessageRole::Tool | MessageRole::User => "user",
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

    // OpenAI vision models often reply with a generic "I don't have access
    // to attached files" when an image content block arrives without any
    // accompanying text block. Always lead with a text part — empty caption
    // becomes a neutral placeholder so the model treats the image as part of
    // a user turn rather than as a standalone, contextless payload.
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
    let url = format!("data:{media_type};base64,{data}");
    Some(json!({
        "type": "image_url",
        "image_url": { "url": url }
    }))
}

/// Translate a document attachment into a Chat Completions content block.
/// PDFs become a native `file` block; text-like documents (.txt, .md, .csv,
/// source code, etc.) are read off disk and inlined as a fenced text block.
/// Pure binary formats (.docx, .xlsx, .zip) return `None` and fall back to
/// the text descriptor — Chat Completions has no generic binary shape.
fn document_block(attachment: &Attachment) -> Option<Value> {
    if let Some(media_type) = document_mime(attachment) {
        if let Ok(data) = read_base64(&attachment.path) {
            let file_data = format!("data:{media_type};base64,{data}");
            return Some(json!({
                "type": "file",
                "file": {
                    "filename": attachment.name.as_ref(),
                    "file_data": file_data,
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
    use agentos_proto::AttachmentKind;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn write_tmp(name: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("openai-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn text_only_keeps_string_content() {
        let msg = Message::text(MessageRole::User, "hi");
        let value = build_message(&msg);
        assert_eq!(value.get("content").and_then(Value::as_str), Some("hi"));
    }

    #[test]
    fn image_emits_image_url_data_uri() {
        let path = write_tmp("frame.png", &[0x89, 0x50, 0x4e, 0x47]);
        let msg = Message {
            role: MessageRole::User,
            content: Arc::from(""),
            attachments: vec![Attachment {
                kind: AttachmentKind::Image,
                name: Arc::from("frame.png"),
                path,
                mime: Some(Arc::from("image/png")),
                size: Some(4),
                source: None,
            }],
            metadata: Default::default(),
        };
        let value = build_message(&msg);
        let blocks = value.get("content").and_then(Value::as_array).unwrap();
        assert_eq!(blocks.len(), 2);
        // Empty caption gets replaced with a neutral placeholder so the model
        // sees a well-formed turn (text + image) rather than image-only.
        assert_eq!(blocks[0]["type"], "text");
        assert!(blocks[0]["text"]
            .as_str()
            .unwrap()
            .contains("without a caption"));
        assert_eq!(blocks[1]["type"], "image_url");
        let url = blocks[1]["image_url"]["url"].as_str().unwrap();
        assert!(url.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn image_with_caption_uses_caption_as_text_block() {
        let path = write_tmp("photo.jpg", &[0xff, 0xd8, 0xff]);
        let msg = Message {
            role: MessageRole::User,
            content: Arc::from("what's in here?"),
            attachments: vec![Attachment {
                kind: AttachmentKind::Image,
                name: Arc::from("photo.jpg"),
                path,
                mime: Some(Arc::from("image/jpeg")),
                size: Some(3),
                source: None,
            }],
            metadata: Default::default(),
        };
        let value = build_message(&msg);
        let blocks = value.get("content").and_then(Value::as_array).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["text"], "what's in here?");
        assert_eq!(blocks[1]["type"], "image_url");
    }

    #[test]
    fn pdf_attachment_emits_file_block() {
        let path = write_tmp("spec.pdf", b"%PDF-1.4 fake");
        let msg = Message {
            role: MessageRole::User,
            content: Arc::from("see attached"),
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
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["text"], "see attached");
        assert_eq!(blocks[1]["type"], "file");
        assert_eq!(blocks[1]["file"]["filename"], "spec.pdf");
        let file_data = blocks[1]["file"]["file_data"].as_str().unwrap();
        assert!(file_data.starts_with("data:application/pdf;base64,"));
    }

    #[test]
    fn text_document_inlined_as_fenced_block() {
        let path = write_tmp("notes.md", b"# heading\nbody text");
        let msg = Message {
            role: MessageRole::User,
            content: Arc::from("summarize"),
            attachments: vec![Attachment {
                kind: AttachmentKind::Document,
                name: Arc::from("notes.md"),
                path,
                mime: None,
                size: Some(19),
                source: None,
            }],
            metadata: Default::default(),
        };
        let value = build_message(&msg);
        let blocks = value.get("content").and_then(Value::as_array).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["text"], "summarize");
        assert_eq!(blocks[1]["type"], "text");
        let inlined = blocks[1]["text"].as_str().unwrap();
        assert!(inlined.starts_with("File: notes.md"));
        assert!(inlined.contains("# heading"));
    }

    #[test]
    fn non_pdf_binary_document_falls_back_to_descriptor() {
        let msg = Message {
            role: MessageRole::User,
            content: Arc::from("see attached"),
            attachments: vec![Attachment {
                kind: AttachmentKind::Document,
                name: Arc::from("notes.docx"),
                path: PathBuf::from("/tmp/notes.docx"),
                mime: Some(Arc::from(
                    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                )),
                size: Some(2048),
                source: None,
            }],
            metadata: Default::default(),
        };
        let value = build_message(&msg);
        let blocks = value.get("content").and_then(Value::as_array).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "text");
        let text = blocks[0]["text"].as_str().unwrap();
        assert!(text.contains("[attached document:"));
        assert!(text.contains("notes.docx"));
    }
}
