use crate::providers::content::{append_descriptors, image_mime, read_base64};
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

    let mut blocks: Vec<Value> = Vec::with_capacity(inline_blocks.len() + 1);
    if !leading_text.is_empty() {
        blocks.push(json!({ "type": "text", "text": leading_text }));
    }
    blocks.extend(inline_blocks);
    if blocks.is_empty() {
        blocks.push(json!({ "type": "text", "text": "" }));
    }

    json!({
        "role": role,
        "content": blocks,
    })
}

fn content_block_for(attachment: &Attachment) -> Option<Value> {
    match attachment.kind {
        AttachmentKind::Image => image_block(attachment),
        // OpenAI chat completions doesn't accept inline document payloads;
        // fall back to a text descriptor so the model at least knows it
        // exists and can call a filesystem tool to read it.
        AttachmentKind::Document => None,
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
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "image_url");
        let url = blocks[0]["image_url"]["url"].as_str().unwrap();
        assert!(url.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn document_falls_back_to_descriptor() {
        let msg = Message {
            role: MessageRole::User,
            content: Arc::from("see attached"),
            attachments: vec![Attachment {
                kind: AttachmentKind::Document,
                name: Arc::from("spec.pdf"),
                path: PathBuf::from("/tmp/spec.pdf"),
                mime: Some(Arc::from("application/pdf")),
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
        assert!(text.starts_with("see attached"));
        assert!(text.contains("[attached document:"));
        assert!(text.contains("spec.pdf"));
    }
}
