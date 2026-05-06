use agentos_interfaces::orchestrator::{Orchestrator, OrchestratorError, Plan, RunContext};
use agentos_llm::Llm;
use agentos_proto::{Message, MessageRole};
use async_trait::async_trait;
use std::sync::Arc;

pub struct EchoOrchestrator;

#[async_trait]
impl Orchestrator for EchoOrchestrator {
    async fn plan(&self, ctx: &RunContext<'_>) -> Result<Plan, OrchestratorError> {
        let content = ctx
            .state
            .transcript
            .items
            .last()
            .map(|item| Arc::clone(&item.message.content))
            .unwrap_or_else(|| Arc::from(""));
        Ok(Plan::Reply(Message::text(MessageRole::Assistant, content)))
    }
}

pub struct MinOrchestrator {
    llm: Arc<dyn Llm>,
}

impl MinOrchestrator {
    pub fn new(llm: Arc<dyn Llm>) -> Self {
        Self { llm }
    }
}

#[async_trait]
impl Orchestrator for MinOrchestrator {
    async fn plan(&self, ctx: &RunContext<'_>) -> Result<Plan, OrchestratorError> {
        Ok(Plan::Reply(self.llm.complete(ctx).await.map_err(
            |err| OrchestratorError::Backend(Arc::from(err.to_string())),
        )?))
    }
}
