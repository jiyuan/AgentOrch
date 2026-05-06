use crate::providers::post_json;
use serde_json::Value;
use std::env;

pub fn complete(model: &str, messages: &[Value]) -> Result<String, String> {
    let host = env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".to_owned());
    let payload = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": false
    });
    let response = post_json(
        "llm",
        &format!("{}/api/chat", host.trim_end_matches('/')),
        &[("Content-Type", "application/json".to_owned())],
        &payload,
    )?;
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
