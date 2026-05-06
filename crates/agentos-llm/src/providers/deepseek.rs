use crate::providers::{format_provider_error, post_json};
use serde_json::Value;
use std::env;

pub fn complete(model: &str, messages: &[Value]) -> Result<String, String> {
    let api_key =
        env::var("DEEPSEEK_API_KEY").map_err(|_| "missing DEEPSEEK_API_KEY".to_owned())?;
    let base_url = env::var("AGENTOS_DEEPSEEK_BASE_URL")
        .or_else(|_| env::var("DEEPSEEK_BASE_URL"))
        .or_else(|_| env::var("DEEPSEEK_HOST"))
        .unwrap_or_else(|_| "https://api.deepseek.com".to_owned());
    let payload = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": false
    });
    let response = post_json(
        "llm",
        &format!("{}/chat/completions", base_url.trim_end_matches('/')),
        &[
            ("Authorization", format!("Bearer {api_key}")),
            ("Content-Type", "application/json".to_owned()),
        ],
        &payload,
    )?;
    if let Some(error) = response.body.get("error") {
        return Err(format_provider_error("DeepSeek", &response, error));
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
                "DeepSeek response missing assistant content: {}",
                response.body
            )
        })
}
