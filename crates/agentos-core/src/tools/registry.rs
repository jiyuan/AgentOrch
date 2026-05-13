use super::{
    CronCreatorTool, CronListTool, CronRemoveTool, FileTool, HttpTool, McpTool, MemoryTool,
    ShellTool, SkillValidateTool,
};
use crate::memory::MemoryManager;
use agentos_interfaces::mcp::{McpClient, McpError, McpServer};
use agentos_interfaces::memory::Memory;
use agentos_interfaces::orchestrator::RunContext;
use agentos_interfaces::tool::{Tool, ToolError, ToolSpec};
use agentos_proto::{ToolCall, ToolResult};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ToolRegistryError {
    #[error("unknown tool: {0}")]
    UnknownTool(Arc<str>),
    #[error(transparent)]
    Tool(#[from] ToolError),
    #[error(transparent)]
    Mcp(#[from] McpError),
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: BTreeMap<Arc<str>, Arc<dyn Tool>>,
    isolation_runner: Option<PathBuf>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reference() -> Self {
        let mut registry = Self::new();
        registry.register(ShellTool);
        registry.register(HttpTool);
        registry.register(FileTool);
        registry.register(SkillValidateTool);
        registry.register(CronCreatorTool);
        registry.register(CronListTool);
        registry.register(CronRemoveTool);
        registry
    }

    pub fn reference_with_memory(memory: Arc<dyn Memory>) -> Self {
        let mut registry = Self::reference();
        registry.register(MemoryTool::new(memory));
        registry
    }

    pub fn reference_with_memory_manager(memory_manager: Arc<MemoryManager>) -> Self {
        let mut registry = Self::reference();
        registry.register(MemoryTool::with_manager(memory_manager));
        registry
    }

    pub fn register<T>(&mut self, tool: T)
    where
        T: Tool + 'static,
    {
        let spec = tool.spec();
        self.tools.insert(spec.name, Arc::new(tool));
    }

    pub fn with_subprocess_isolation(mut self, runner: impl Into<PathBuf>) -> Self {
        self.isolation_runner = Some(runner.into());
        self
    }

    pub async fn register_mcp_server(
        &mut self,
        server: McpServer,
        client: Arc<dyn McpClient>,
    ) -> Result<Vec<ToolSpec>, ToolRegistryError> {
        let specs = client.list_tools(&server).await?;
        for spec in specs.iter().cloned() {
            self.register(McpTool::new(server.clone(), Arc::clone(&client), spec));
        }
        Ok(specs)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|tool| tool.spec()).collect()
    }

    pub async fn call(&self, call: &ToolCall) -> Result<ToolResult, ToolRegistryError> {
        let tool = self
            .tools
            .get(&call.name)
            .ok_or_else(|| ToolRegistryError::UnknownTool(Arc::clone(&call.name)))?;
        let spec = tool.spec();
        if spec.requires_isolation {
            if let Some(runner) = &self.isolation_runner {
                return Ok(call_isolated_subprocess(runner, call)?);
            }
        }
        Ok(tool.call(call, &call.args).await?)
    }

    pub async fn call_with_context(
        &self,
        call: &ToolCall,
        ctx: &RunContext<'_>,
    ) -> Result<ToolResult, ToolRegistryError> {
        let tool = self
            .tools
            .get(&call.name)
            .ok_or_else(|| ToolRegistryError::UnknownTool(Arc::clone(&call.name)))?;
        let spec = tool.spec();
        if spec.requires_isolation {
            if let Some(runner) = &self.isolation_runner {
                return Ok(call_isolated_subprocess(runner, call)?);
            }
        }
        Ok(tool.call_with_context(call, &call.args, ctx).await?)
    }
}

#[derive(serde::Serialize)]
struct IsolatedToolRequest<'a> {
    call: &'a ToolCall,
}

pub fn call_isolated_subprocess(
    runner: &std::path::Path,
    call: &ToolCall,
) -> Result<ToolResult, ToolError> {
    let request = serde_json::to_vec(&IsolatedToolRequest { call })
        .map_err(|err| ToolError::Failed(err.to_string().into()))?;
    let mut child = Command::new(runner)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| ToolError::Failed(err.to_string().into()))?;
    let stdin = child
        .stdin
        .as_mut()
        .ok_or_else(|| ToolError::Failed(Arc::from("isolated worker stdin unavailable")))?;
    use std::io::Write;
    stdin
        .write_all(&request)
        .map_err(|err| ToolError::Failed(err.to_string().into()))?;
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .map_err(|err| ToolError::Failed(err.to_string().into()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr.trim();
        return Err(ToolError::Failed(Arc::from(if message.is_empty() {
            "isolated worker failed".to_owned()
        } else {
            message.to_owned()
        })));
    }

    let mut result: ToolResult = serde_json::from_slice(&output.stdout)
        .map_err(|err| ToolError::Failed(err.to_string().into()))?;
    result.metadata.insert(
        Arc::from("isolation"),
        Value::String("subprocess".to_owned()),
    );
    result.metadata.insert(
        Arc::from("isolation_runner"),
        Value::String(runner.display().to_string()),
    );
    Ok(result)
}
