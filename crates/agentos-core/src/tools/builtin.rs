use agentos_interfaces::tool::{Tool, ToolError, ToolSpec};
use agentos_proto::{ToolCall, ToolResult, ToolStatus};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, value::RawValue, Value};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct ShellTool;

#[derive(Debug, Deserialize)]
struct ShellArgs {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    cwd: Option<PathBuf>,
}

#[async_trait]
impl Tool for ShellTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: Arc::from("shell"),
            description: Arc::from("Run an allowlisted shell command with structured arguments."),
            input_schema: json!({
                "type": "object",
                "required": ["command"],
                "properties": {
                    "command": { "type": "string" },
                    "args": { "type": "array", "items": { "type": "string" } },
                    "cwd": { "type": "string" }
                }
            }),
            requires_isolation: true,
        }
    }

    async fn call(&self, call: &ToolCall, args: &RawValue) -> Result<ToolResult, ToolError> {
        let parsed: ShellArgs = serde_json::from_str(args.get())
            .map_err(|err| ToolError::Failed(err.to_string().into()))?;
        let start = Instant::now();
        let mut command = Command::new(&parsed.command);
        command.args(&parsed.args);
        if let Some(cwd) = parsed.cwd {
            command.current_dir(cwd);
        }

        let output = command
            .output()
            .map_err(|err| ToolError::Failed(err.to_string().into()))?;
        let duration_ms = elapsed_ms(start);
        let mut content = String::from_utf8_lossy(&output.stdout).into_owned();
        if content.is_empty() {
            content = String::from_utf8_lossy(&output.stderr).into_owned();
        }
        let bytes_out = content.len() as u64;
        let mut metadata = result_metadata(duration_ms, bytes_out);
        metadata.insert(
            Arc::from("exit_code"),
            output
                .status
                .code()
                .map_or(Value::Null, |code| Value::from(code as i64)),
        );
        metadata.insert(
            Arc::from("stderr_bytes"),
            Value::from(output.stderr.len() as u64),
        );

        Ok(ToolResult {
            call_id: call.id.clone(),
            status: if output.status.success() {
                ToolStatus::Succeeded
            } else {
                ToolStatus::Failed
            },
            content: Arc::from(content),
            metadata,
        })
    }
}

#[derive(Default)]
pub struct FileTool;

#[derive(Debug, Deserialize)]
struct FileArgs {
    operation: String,
    path: PathBuf,
    #[serde(default)]
    content: Option<String>,
}

#[async_trait]
impl Tool for FileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: Arc::from("file"),
            description: Arc::from("Read or write a UTF-8 file."),
            input_schema: json!({
                "type": "object",
                "required": ["operation", "path"],
                "properties": {
                    "operation": { "type": "string", "enum": ["read", "write"] },
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                }
            }),
            requires_isolation: false,
        }
    }

    async fn call(&self, call: &ToolCall, args: &RawValue) -> Result<ToolResult, ToolError> {
        let parsed: FileArgs = serde_json::from_str(args.get())
            .map_err(|err| ToolError::Failed(err.to_string().into()))?;
        let start = Instant::now();
        match parsed.operation.as_str() {
            "read" => {
                let content = std::fs::read_to_string(&parsed.path)
                    .map_err(|err| ToolError::Failed(err.to_string().into()))?;
                let bytes_out = content.len() as u64;
                Ok(ToolResult {
                    call_id: call.id.clone(),
                    status: ToolStatus::Succeeded,
                    content: Arc::from(content),
                    metadata: result_metadata(elapsed_ms(start), bytes_out),
                })
            }
            "write" => {
                let content = parsed.content.unwrap_or_default();
                std::fs::write(&parsed.path, content.as_bytes())
                    .map_err(|err| ToolError::Failed(err.to_string().into()))?;
                let message = format!("wrote {} bytes", content.len());
                Ok(ToolResult {
                    call_id: call.id.clone(),
                    status: ToolStatus::Succeeded,
                    content: Arc::from(message),
                    metadata: result_metadata(elapsed_ms(start), content.len() as u64),
                })
            }
            operation => Err(ToolError::Failed(
                format!("unsupported file operation: {operation}").into(),
            )),
        }
    }
}

#[derive(Default)]
pub struct HttpTool;

#[derive(Debug, Deserialize)]
struct HttpArgs {
    url: String,
    #[serde(default = "default_get")]
    method: String,
}

#[async_trait]
impl Tool for HttpTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: Arc::from("http"),
            description: Arc::from("Fetch an HTTP or HTTPS URL with a GET request."),
            input_schema: json!({
                "type": "object",
                "required": ["url"],
                "properties": {
                    "url": { "type": "string" },
                    "method": { "type": "string", "enum": ["GET"] }
                }
            }),
            requires_isolation: false,
        }
    }

    async fn call(&self, call: &ToolCall, args: &RawValue) -> Result<ToolResult, ToolError> {
        let parsed: HttpArgs = serde_json::from_str(args.get())
            .map_err(|err| ToolError::Failed(err.to_string().into()))?;
        if !parsed.method.eq_ignore_ascii_case("GET") {
            return Err(ToolError::Failed(Arc::from("http tool only supports GET")));
        }
        if parsed.url.starts_with("https://") {
            return fetch_https_with_curl(call, &parsed.url);
        }

        let target = parse_http_url(&parsed.url)?;
        let start = Instant::now();
        let mut stream = TcpStream::connect((target.host.as_str(), target.port))
            .map_err(|err| ToolError::Failed(err.to_string().into()))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(|err| ToolError::Failed(err.to_string().into()))?;
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .map_err(|err| ToolError::Failed(err.to_string().into()))?;
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: agentos-core/0.1\r\nConnection: close\r\n\r\n",
            target.path, target.host
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|err| ToolError::Failed(err.to_string().into()))?;
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .map_err(|err| ToolError::Failed(err.to_string().into()))?;
        let (status_line, body) = split_http_response(&response);
        let bytes_out = body.len() as u64;
        let mut metadata = result_metadata(elapsed_ms(start), bytes_out);
        metadata.insert(Arc::from("status_line"), Value::String(status_line));

        Ok(ToolResult {
            call_id: call.id.clone(),
            status: http_status(&metadata).map_or(ToolStatus::Succeeded, status_from_http_code),
            content: Arc::from(body),
            metadata,
        })
    }
}

struct HttpTarget {
    host: String,
    port: u16,
    path: String,
}

fn parse_http_url(url: &str) -> Result<HttpTarget, ToolError> {
    let Some(rest) = url.strip_prefix("http://") else {
        return Err(ToolError::Failed(Arc::from(
            "http tool currently supports http:// URLs only",
        )));
    };
    let (host_port, path) = rest.split_once('/').unwrap_or((rest, ""));
    if host_port.is_empty() {
        return Err(ToolError::Failed(Arc::from("http URL host is empty")));
    }
    let (host, port) = match host_port.rsplit_once(':') {
        Some((host, port)) => {
            let parsed_port = port
                .parse::<u16>()
                .map_err(|err| ToolError::Failed(err.to_string().into()))?;
            (host.to_owned(), parsed_port)
        }
        None => (host_port.to_owned(), 80),
    };

    Ok(HttpTarget {
        host,
        port,
        path: format!("/{path}"),
    })
}

fn split_http_response(response: &str) -> (String, String) {
    let status_line = response.lines().next().unwrap_or_default().to_owned();
    let body = response
        .split_once("\r\n\r\n")
        .map_or_else(String::new, |(_, body)| body.to_owned());
    (status_line, body)
}

fn fetch_https_with_curl(call: &ToolCall, url: &str) -> Result<ToolResult, ToolError> {
    let start = Instant::now();
    let output = Command::new("curl")
        .args([
            "--location",
            "--silent",
            "--show-error",
            "--max-time",
            "10",
            "--write-out",
            "\nagentos_http_status:%{http_code}",
            url,
        ])
        .output()
        .map_err(|err| ToolError::Failed(err.to_string().into()))?;

    let mut content = String::from_utf8_lossy(&output.stdout).into_owned();
    let split_status = content
        .rsplit_once("\nagentos_http_status:")
        .map(|(body, code)| (body.to_owned(), code.trim().parse::<u16>().ok()));
    let http_code = if let Some((body, code)) = split_status {
        content = body;
        code
    } else {
        None
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ToolError::Failed(stderr.trim().to_owned().into()));
    }

    let bytes_out = content.len() as u64;
    let mut metadata = result_metadata(elapsed_ms(start), bytes_out);
    metadata.insert(
        Arc::from("status_line"),
        Value::String(http_code.map_or_else(
            || "curl status unknown".to_owned(),
            |code| format!("curl status {code}"),
        )),
    );

    Ok(ToolResult {
        call_id: call.id.clone(),
        status: http_code.map_or(ToolStatus::Succeeded, status_from_http_code),
        content: Arc::from(content),
        metadata,
    })
}

fn http_status(metadata: &BTreeMap<Arc<str>, Value>) -> Option<u16> {
    let status_line = metadata.get("status_line")?.as_str()?;
    status_line
        .split_whitespace()
        .find_map(|part| part.parse::<u16>().ok())
}

fn status_from_http_code(code: u16) -> ToolStatus {
    if (200..300).contains(&code) {
        ToolStatus::Succeeded
    } else {
        ToolStatus::Failed
    }
}

fn default_get() -> String {
    "GET".to_owned()
}

fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

fn result_metadata(duration_ms: u64, bytes_out: u64) -> BTreeMap<Arc<str>, Value> {
    let mut metadata = BTreeMap::new();
    metadata.insert(Arc::from("duration_ms"), Value::from(duration_ms));
    metadata.insert(Arc::from("bytes_out"), Value::from(bytes_out));
    metadata
}
