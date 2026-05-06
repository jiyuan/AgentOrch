use agentos_proto::{Message, ToolCall, ToolResult};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;

use crate::orchestrator::RunContext;

#[derive(Debug, Error)]
pub enum GuardrailError {
    #[error("guardrail backend failed: {0}")]
    Backend(Arc<str>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Input {
    pub message: Message,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "reason")]
pub enum GuardrailOutcome {
    Passed,
    Tripped(Arc<str>),
}

#[async_trait]
pub trait InputGuardrail: Send + Sync {
    /// Check initial user input before the first planning step.
    async fn check(
        &self,
        input: &Input,
        ctx: &RunContext<'_>,
    ) -> Result<GuardrailOutcome, GuardrailError>;
}

#[async_trait]
pub trait OutputGuardrail: Send + Sync {
    /// Check terminal assistant output before the loop finishes.
    async fn check(
        &self,
        output: &Message,
        ctx: &RunContext<'_>,
    ) -> Result<GuardrailOutcome, GuardrailError>;
}

#[async_trait]
pub trait ToolGuardrail: Send + Sync {
    /// Check a tool call before execution and its result after execution.
    async fn check_call(
        &self,
        call: &ToolCall,
        ctx: &RunContext<'_>,
    ) -> Result<GuardrailOutcome, GuardrailError>;

    /// Check the result of a tool call before it enters the transcript.
    async fn check_result(
        &self,
        result: &ToolResult,
        ctx: &RunContext<'_>,
    ) -> Result<GuardrailOutcome, GuardrailError>;
}
