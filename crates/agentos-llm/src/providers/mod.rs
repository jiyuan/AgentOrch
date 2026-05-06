pub mod anthropic;
pub mod deepseek;
pub mod ollama;
pub mod openai;

use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::process::{self, Command};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub(crate) struct JsonHttpResponse {
    pub status: Option<u16>,
    pub headers: BTreeMap<String, String>,
    pub body: Value,
}

pub(crate) fn post_json(
    header_prefix: &str,
    url: &str,
    headers: &[(&str, String)],
    payload: &Value,
) -> Result<JsonHttpResponse, String> {
    let payload = serde_json::to_string(payload)
        .map_err(|err| format!("failed to encode LLM request: {err}"))?;
    let header_path = env::temp_dir().join(format!(
        "agentos-{header_prefix}-headers-{}-{}.txt",
        process::id(),
        unix_timestamp()
    ));
    let mut command = Command::new("curl");
    command.args([
        "--silent",
        "--show-error",
        "--max-time",
        "60",
        "--dump-header",
    ]);
    command.arg(&header_path).args(["-X", "POST"]);
    for (name, value) in headers {
        command.args(["-H", &format!("{name}: {value}")]);
    }
    command.arg(url).args(["-d", &payload]);
    let output = command
        .output()
        .map_err(|err| format!("failed to invoke curl for LLM request: {err}"))?;
    let raw_headers = fs::read_to_string(&header_path).unwrap_or_default();
    let _ = fs::remove_file(&header_path);
    let (status, headers) = parse_http_headers(&raw_headers);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "LLM request failed: {}; {}",
            stderr.trim(),
            describe_http_response(status, &headers)
        ));
    }
    let body = serde_json::from_slice(&output.stdout).map_err(|err| {
        format!(
            "failed to parse LLM response: {err}; {}; body={}",
            describe_http_response(status, &headers),
            String::from_utf8_lossy(&output.stdout),
        )
    })?;
    Ok(JsonHttpResponse {
        status,
        headers,
        body,
    })
}

pub(crate) fn first_env<const N: usize>(names: [&str; N]) -> Option<String> {
    names.into_iter().find_map(|name| env::var(name).ok())
}

pub(crate) fn format_openai_error(response: &JsonHttpResponse, error: &Value) -> String {
    format_provider_error_with_hint("OpenAI", response, error, openai_quota_hint(error))
}

pub(crate) fn format_provider_error(
    provider: &str,
    response: &JsonHttpResponse,
    error: &Value,
) -> String {
    format_provider_error_with_hint(
        provider,
        response,
        error,
        "inspect the provider API error code and request id",
    )
}

fn format_provider_error_with_hint(
    provider: &str,
    response: &JsonHttpResponse,
    error: &Value,
    hint: &str,
) -> String {
    format!(
        "{provider} error: {}; {}; hint={}",
        error,
        describe_http_response(response.status, &response.headers),
        hint,
    )
}

fn parse_http_headers(raw: &str) -> (Option<u16>, BTreeMap<String, String>) {
    let mut status = None;
    let mut headers = BTreeMap::new();
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with("HTTP/") {
            status = line
                .split_whitespace()
                .nth(1)
                .and_then(|code| code.parse::<u16>().ok());
            headers.clear();
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }
    (status, headers)
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn describe_http_response(status: Option<u16>, headers: &BTreeMap<String, String>) -> String {
    let mut parts = Vec::new();
    if let Some(status) = status {
        parts.push(format!("http_status={status}"));
    }
    for name in [
        "x-request-id",
        "openai-organization",
        "x-ratelimit-remaining-requests",
        "x-ratelimit-remaining-tokens",
    ] {
        if let Some(value) = headers.get(name) {
            parts.push(format!("{name}={value}"));
        }
    }
    if parts.is_empty() {
        "http_metadata=unavailable".to_owned()
    } else {
        parts.join(", ")
    }
}

fn openai_quota_hint(error: &Value) -> &'static str {
    if error.get("code").and_then(Value::as_str) == Some("insufficient_quota") {
        return "check the OpenAI Platform project/org tied to this API key, project monthly budget, org usage limit, and prepaid API credits";
    }
    "inspect the OpenAI API error code and request id"
}
