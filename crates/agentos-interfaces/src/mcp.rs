use crate::tool::ToolSpec;
use agentos_proto::{ToolCall, ToolResult};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum McpError {
    #[error("mcp server failed: {0}")]
    Failed(Arc<str>),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct McpServer {
    pub id: Arc<str>,
    pub endpoint: Arc<str>,
}

#[async_trait]
pub trait McpClient: Send + Sync {
    /// List tools exposed by a remote MCP server.
    async fn list_tools(&self, server: &McpServer) -> Result<Vec<ToolSpec>, McpError>;

    /// Invoke a remote MCP-backed tool.
    ///
    /// Core approval still applies to the resulting tool call.
    async fn call_tool(&self, server: &McpServer, call: &ToolCall) -> Result<ToolResult, McpError>;
}
