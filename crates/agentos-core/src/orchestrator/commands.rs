use agentos_interfaces::orchestrator::{OrchestratorError, Plan, SubAgentSpec};
use agentos_proto::{AgentId, ToolCall, ToolCallId};
use serde_json::{json, value::RawValue};
use std::collections::BTreeMap;
use std::sync::Arc;

pub(super) fn deterministic_plan_from_user_text(
    input: &str,
) -> Result<Option<Plan>, OrchestratorError> {
    if let Some(rest) = input.strip_prefix("delegate ") {
        if let Some(plan) = delegate_plan(rest) {
            return Ok(Some(plan));
        }
    }
    if let Some(rest) = input.strip_prefix("tool ") {
        if let Some(plan) = generic_tool_plan(rest)? {
            return Ok(Some(plan));
        }
    }
    if let Some(command) = input.strip_prefix("shell:") {
        return shell_plan(command.trim()).map(Some);
    }
    if let Some(fact) = input.strip_prefix("remember:") {
        let fact = fact.trim();
        return raw_tool_plan(
            "memory",
            json!({
                "operation": "write",
                "body": {
                    "fact": fact
                }
            }),
        )
        .map(Some);
    }
    if let Some(query) = input.strip_prefix("recall:") {
        return raw_tool_plan(
            "memory",
            json!({
                "operation": "read",
                "text": query.trim(),
                "limit": 5
            }),
        )
        .map(Some);
    }
    if let Some(path) = input.strip_prefix("read file:") {
        return raw_tool_plan(
            "file",
            json!({
                "operation": "read",
                "path": path.trim()
            }),
        )
        .map(Some);
    }
    if let Some(url) = input.strip_prefix("http get:") {
        return raw_tool_plan(
            "http",
            json!({
                "method": "GET",
                "url": url.trim()
            }),
        )
        .map(Some);
    }

    Ok(None)
}

fn generic_tool_plan(input: &str) -> Result<Option<Plan>, OrchestratorError> {
    let Some((name, input)) = input.split_once(':') else {
        return Ok(None);
    };
    Ok(Some(raw_tool_plan(
        name.trim(),
        json!({
            "input": input.trim()
        }),
    )?))
}

fn delegate_plan(input: &str) -> Option<Plan> {
    let (target, prompt) = input.split_once(':')?;
    let mut parts = target.split_whitespace();
    let agent_id = parts.next()?;
    let policy_id = parts.next().unwrap_or("default");
    let prompt = prompt.trim();
    let mut metadata = BTreeMap::new();
    metadata.insert(Arc::from("prompt"), json!(prompt));
    Some(Plan::Delegate(SubAgentSpec {
        agent_id: AgentId::new(agent_id),
        policy_id: Arc::from(policy_id),
        metadata,
    }))
}

fn shell_plan(input: &str) -> Result<Plan, OrchestratorError> {
    let mut parts = input.split_whitespace();
    let command = parts.next().unwrap_or_default();
    raw_tool_plan(
        "shell",
        json!({
            "command": command,
            "args": parts.collect::<Vec<_>>()
        }),
    )
}

fn raw_tool_plan(name: &str, args: serde_json::Value) -> Result<Plan, OrchestratorError> {
    let raw_args = RawValue::from_string(args.to_string())
        .map_err(|err| OrchestratorError::Backend(err.to_string().into()))?;
    Ok(Plan::CallTool(ToolCall {
        id: ToolCallId::new("call-1"),
        name: Arc::from(name),
        args: raw_args,
    }))
}
