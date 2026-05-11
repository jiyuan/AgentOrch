use crate::providers::content::append_descriptors;
use crate::providers::post_json;
use agentos_proto::{Message, MessageRole};
use serde_json::{json, Value};
use std::env;

pub async fn complete(model: &str, messages: &[Message]) -> Result<String, String> {
    let host = env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".to_owned());
    let serialized = messages.iter().map(flat_message).collect::<Vec<_>>();
    let payload = json!({
        "model": model,
        "messages": serialized,
        "stream": false
    });
    let response = post_json(
        "llm",
        &format!("{}/api/chat", host.trim_end_matches('/')),
        &[("Content-Type", "application/json".to_owned())],
        &payload,
    )
    .await?;
    response
        .body
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            format!(
                "Ollama response missing assistant content: {}",
                response.body
            )
        })
}

fn flat_message(message: &Message) -> Value {
    let role = match message.role {
        MessageRole::Assistant => "assistant",
        MessageRole::System => "system",
        MessageRole::Tool | MessageRole::User => "user",
    };
    let base = if message.role == MessageRole::Tool {
        format!("Tool result: {}", message.content)
    } else {
        message.content.to_string()
    };
    let content = append_descriptors(&base, &message.attachments);
    json!({ "role": role, "content": content })
}
