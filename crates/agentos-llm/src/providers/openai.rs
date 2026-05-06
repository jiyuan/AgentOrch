use crate::providers::{first_env, format_openai_error, post_json};
use serde_json::Value;
use std::env;

pub fn complete(model: &str, messages: &[Value]) -> Result<String, String> {
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
    let payload = serde_json::json!({
        "model": model,
        "messages": messages,
        "temperature": 0.7
    });
    let response = post_json(
        "llm",
        &format!("{}/chat/completions", base_url.trim_end_matches('/')),
        &headers,
        &payload,
    )?;
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
