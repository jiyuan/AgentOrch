use agentos_proto::ToolResult;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{value::RawValue, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SkillError {
    #[error("skill failed: {0}")]
    Failed(Arc<str>),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SkillInvocation {
    pub name: Arc<str>,
    pub args: Box<RawValue>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<Arc<str>, Value>,
}

#[async_trait]
pub trait Skill: Send + Sync {
    /// Return the skill name used by orchestrators and registries.
    fn name(&self) -> Arc<str>;

    /// Execute a composed workflow.
    ///
    /// Skills may call tools through approved runtime paths, but must not
    /// bypass the core loop's guardrail or approval checks.
    async fn invoke(&self, invocation: &SkillInvocation) -> Result<ToolResult, SkillError>;
}
