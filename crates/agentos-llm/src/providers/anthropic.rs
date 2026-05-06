use crate::providers::post_json;
use serde_json::Value;
use std::env;

pub fn complete(model: &str, messages: &[Value]) -> Result<String, String> {
    let api_key =
        env::var("ANTHROPIC_API_KEY").map_err(|_| "missing ANTHROPIC_API_KEY".to_owned())?;
    let base_url = env::var("AGENTOS_ANTHROPIC_BASE_URL")
        .or_else(|_| env::var("ANTHROPIC_BASE_URL"))
        .unwrap_or_else(|_| "https://api.anthropic.com/v1".to_owned());
    let messages = messages
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) != Some("system"))
        .cloned()
        .collect::<Vec<_>>();
    let payload = serde_json::json!({
        "model": model,
        "max_tokens": 1024,
        "messages": messages
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
    )?;
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
