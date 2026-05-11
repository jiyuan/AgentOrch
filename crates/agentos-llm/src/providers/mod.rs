pub mod anthropic;
pub(crate) mod content;
pub mod deepseek;
pub mod ollama;
pub mod openai;

use rand::Rng;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE};
use reqwest::{Client, StatusCode};
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::time::sleep;

#[derive(Debug)]
pub(crate) struct JsonHttpResponse {
    pub status: Option<u16>,
    pub headers: BTreeMap<String, String>,
    pub body: Value,
}

const MAX_ATTEMPTS: u32 = 5;
const BASE_BACKOFF: Duration = Duration::from_millis(250);
const MAX_BACKOFF: Duration = Duration::from_secs(8);
const RETRY_AFTER_CAP: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

fn shared_client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .pool_idle_timeout(Duration::from_secs(90))
            .user_agent(concat!("agentos-llm/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client builds with rustls + http2 features compiled in")
    })
}

pub(crate) async fn post_json(
    _header_prefix: &str,
    url: &str,
    headers: &[(&str, String)],
    payload: &Value,
) -> Result<JsonHttpResponse, String> {
    let body = serde_json::to_vec(payload)
        .map_err(|err| format!("failed to encode LLM request: {err}"))?;
    let header_map = build_header_map(headers)?;
    let client = shared_client();

    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        match send_once(client, url, &header_map, &body).await {
            Ok(response) => {
                if attempt < MAX_ATTEMPTS && is_retryable_status(response.status) {
                    let retry_after = parse_retry_after(&response.headers);
                    sleep(backoff_delay(attempt - 1, retry_after)).await;
                    continue;
                }
                return Ok(response);
            }
            Err(message) => {
                if attempt < MAX_ATTEMPTS {
                    sleep(backoff_delay(attempt - 1, None)).await;
                    continue;
                }
                return Err(message);
            }
        }
    }
}

async fn send_once(
    client: &Client,
    url: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<JsonHttpResponse, String> {
    let response = client
        .post(url)
        .headers(headers.clone())
        .body(body.to_vec())
        .send()
        .await
        .map_err(|err| format!("LLM request failed: {err}; http_metadata=unavailable"))?;
    let status = response.status().as_u16();
    let header_map = collect_headers(response.headers());
    let bytes = response.bytes().await.map_err(|err| {
        format!(
            "LLM request failed: {err}; {}",
            describe_http_response(Some(status), &header_map)
        )
    })?;
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).map_err(|err| {
            format!(
                "failed to parse LLM response: {err}; {}; body={}",
                describe_http_response(Some(status), &header_map),
                String::from_utf8_lossy(&bytes),
            )
        })?
    };
    Ok(JsonHttpResponse {
        status: Some(status),
        headers: header_map,
        body,
    })
}

fn build_header_map(headers: &[(&str, String)]) -> Result<HeaderMap, String> {
    let mut map = HeaderMap::with_capacity(headers.len() + 1);
    for (name, value) in headers {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|err| format!("invalid header name {name}: {err}"))?;
        let header_value = HeaderValue::from_str(value)
            .map_err(|err| format!("invalid header value for {name}: {err}"))?;
        map.insert(header_name, header_value);
    }
    if !map.contains_key(CONTENT_TYPE) {
        map.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    }
    Ok(map)
}

fn collect_headers(map: &HeaderMap) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, value) in map.iter() {
        if let Ok(text) = value.to_str() {
            out.insert(name.as_str().to_ascii_lowercase(), text.to_owned());
        }
    }
    out
}

fn is_retryable_status(status: Option<u16>) -> bool {
    match status {
        Some(code) => {
            code == StatusCode::TOO_MANY_REQUESTS.as_u16()
                || code == StatusCode::REQUEST_TIMEOUT.as_u16()
                || (500..=599).contains(&code)
        }
        None => false,
    }
}

fn parse_retry_after(headers: &BTreeMap<String, String>) -> Option<Duration> {
    let value = headers.get("retry-after")?.trim();
    value
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
        .map(|delay| delay.min(RETRY_AFTER_CAP))
}

fn backoff_delay(attempt: u32, retry_after: Option<Duration>) -> Duration {
    if let Some(after) = retry_after {
        return after;
    }
    let multiplier = 2_u32.saturating_pow(attempt);
    let exp = BASE_BACKOFF.saturating_mul(multiplier).min(MAX_BACKOFF);
    let upper = exp.as_nanos().min(u64::MAX as u128) as u64;
    if upper == 0 {
        return Duration::ZERO;
    }
    let jitter = rand::thread_rng().gen_range(0..=upper);
    Duration::from_nanos(jitter)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_parses_seconds() {
        let mut headers = BTreeMap::new();
        headers.insert("retry-after".to_owned(), "3".to_owned());
        assert_eq!(parse_retry_after(&headers), Some(Duration::from_secs(3)));
    }

    #[test]
    fn retry_after_caps_long_waits() {
        let mut headers = BTreeMap::new();
        headers.insert("retry-after".to_owned(), "9000".to_owned());
        assert_eq!(parse_retry_after(&headers), Some(RETRY_AFTER_CAP));
    }

    #[test]
    fn retry_after_ignores_http_dates() {
        let mut headers = BTreeMap::new();
        headers.insert(
            "retry-after".to_owned(),
            "Wed, 21 Oct 2026 07:28:00 GMT".to_owned(),
        );
        assert_eq!(parse_retry_after(&headers), None);
    }

    #[test]
    fn retryable_status_covers_throttling_and_5xx() {
        assert!(is_retryable_status(Some(429)));
        assert!(is_retryable_status(Some(408)));
        assert!(is_retryable_status(Some(500)));
        assert!(is_retryable_status(Some(503)));
        assert!(!is_retryable_status(Some(400)));
        assert!(!is_retryable_status(Some(401)));
        assert!(!is_retryable_status(Some(200)));
        assert!(!is_retryable_status(None));
    }

    #[test]
    fn backoff_delay_honors_retry_after_over_exponential() {
        let delay = backoff_delay(3, Some(Duration::from_secs(2)));
        assert_eq!(delay, Duration::from_secs(2));
    }

    #[test]
    fn backoff_delay_grows_within_cap() {
        for attempt in 0..6 {
            let delay = backoff_delay(attempt, None);
            assert!(delay <= MAX_BACKOFF, "attempt {attempt}: {delay:?}");
        }
    }
}
