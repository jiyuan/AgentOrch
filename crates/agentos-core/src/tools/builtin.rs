use crate::http::shared_client;
use agentos_interfaces::tool::{Tool, ToolError, ToolSpec};
use agentos_proto::{ToolCall, ToolResult, ToolStatus};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, value::RawValue, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

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
        if !(parsed.url.starts_with("http://") || parsed.url.starts_with("https://")) {
            return Err(ToolError::Failed(Arc::from(
                "http tool requires an http:// or https:// URL",
            )));
        }

        let start = Instant::now();
        let response = shared_client()
            .get(&parsed.url)
            .send()
            .await
            .map_err(|err| ToolError::Failed(err.to_string().into()))?;
        let status_code = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| ToolError::Failed(err.to_string().into()))?;
        let bytes_out = body.len() as u64;
        let mut metadata = result_metadata(elapsed_ms(start), bytes_out);
        metadata.insert(
            Arc::from("status_line"),
            Value::String(format!(
                "HTTP {} {}",
                status_code.as_u16(),
                status_code.canonical_reason().unwrap_or("")
            )),
        );

        Ok(ToolResult {
            call_id: call.id.clone(),
            status: status_from_http_code(status_code.as_u16()),
            content: Arc::from(body),
            metadata,
        })
    }
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
