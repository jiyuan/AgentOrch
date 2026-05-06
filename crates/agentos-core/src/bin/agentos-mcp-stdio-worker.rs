use agentos_proto::{ToolCall, ToolResult, ToolStatus};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::sync::Arc;

#[derive(Deserialize)]
struct JsonRpcRequest {
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let input = line?;
        if input.trim().is_empty() {
            continue;
        }
        let response = handle_request(&input)?;
        serde_json::to_writer(&mut stdout, &response)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }
    Ok(())
}

fn handle_request(input: &str) -> Result<Value, Box<dyn std::error::Error>> {
    let request: JsonRpcRequest = serde_json::from_str(input)?;
    Ok(match request.method.as_str() {
        "tools/list" => ok_response(
            request.id,
            json!({
                "tools": [{
                    "name": "stdio_echo",
                    "description": "Echo input through a stdio MCP worker.",
                    "input_schema": {
                        "type": "object",
                        "properties": {
                            "input": { "type": "string" },
                            "query": { "type": "string" },
                            "sleep_ms": { "type": "integer" }
                        }
                    },
                    "requires_isolation": false
                }]
            }),
        ),
        "tools/call" => match call_tool(&request.params) {
            Ok(result) => ok_response(request.id, serde_json::to_value(result)?),
            Err(err) => error_response(request.id, &err),
        },
        other => error_response(request.id, &format!("unsupported MCP method: {other}")),
    })
}

fn call_tool(params: &Value) -> Result<ToolResult, String> {
    let call: ToolCall = serde_json::from_value(
        params
            .get("call")
            .cloned()
            .ok_or_else(|| "tools/call params missing call".to_owned())?,
    )
    .map_err(|err| err.to_string())?;
    if call.name.as_ref() != "stdio_echo" {
        return Err(format!("unknown stdio MCP tool: {}", call.name));
    }

    let args: Value = serde_json::from_str(call.args.get()).map_err(|err| err.to_string())?;
    if let Some(sleep_ms) = args.get("sleep_ms").and_then(Value::as_u64) {
        std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
    }
    let content = args
        .get("input")
        .or_else(|| args.get("query"))
        .and_then(Value::as_str)
        .unwrap_or("stdio MCP response");
    let mut metadata = BTreeMap::new();
    metadata.insert(
        Arc::from("mcp_worker"),
        Value::String("agentos-mcp-stdio-worker".to_owned()),
    );
    metadata.insert(
        Arc::from("worker_pid"),
        Value::from(std::process::id() as u64),
    );

    Ok(ToolResult {
        call_id: call.id,
        status: ToolStatus::Succeeded,
        content: Arc::from(content),
        metadata,
    })
}

fn ok_response(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

fn error_response(id: Value, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32000,
            "message": message
        }
    })
}
