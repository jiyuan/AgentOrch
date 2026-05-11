use super::commands::deterministic_plan_from_user_text;
use super::routing::{
    classify_task, latest_user_content, materialize_dispatch, parse_routing_decision,
    routing_classifier_messages, rule_for_domain, rule_for_domain_key, ROUTER_CONFIDENCE_THRESHOLD,
};
use crate::memory::{
    memory_caller_from_context, HydrationRequest, MemoryManager, MemoryStore, RetrievalStrategy,
};
use crate::skills::{WebResearchSkill, WorkspaceSkillCatalog};
use agentos_interfaces::orchestrator::{
    DispatchPriority, Orchestrator, OrchestratorError, Plan, ResourceEntry, ResourceIndex,
    ResourceKind, RoutingRule, RoutingTable, RunContext,
};
use agentos_interfaces::tool::ToolSpec;
use agentos_llm::Llm;
use agentos_proto::{Message, MessageRole};
use async_trait::async_trait;
use std::sync::Arc;

const DEFAULT_MEMORY_HYDRATION_MAX_FRAGMENTS: usize = 5;
const DEFAULT_MEMORY_HYDRATION_MAX_TOKENS: usize = 1_200;

#[derive(Default)]
pub struct MaxOrchestrator {
    available_tools: Vec<ToolSpec>,
    resource_index: ResourceIndex,
    routing_table: RoutingTable,
    llm: Option<Arc<dyn Llm>>,
    memory_hydrator: Option<MemoryHydrator>,
    skill_catalog: WorkspaceSkillCatalog,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryHydrationSettings {
    pub enabled: bool,
    pub max_fragments: usize,
    pub max_estimated_tokens: usize,
    pub stores: Vec<MemoryStore>,
    pub strategy: RetrievalStrategy,
    pub allowed_shared_domains: Vec<Arc<str>>,
}

impl Default for MemoryHydrationSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            max_fragments: DEFAULT_MEMORY_HYDRATION_MAX_FRAGMENTS,
            max_estimated_tokens: DEFAULT_MEMORY_HYDRATION_MAX_TOKENS,
            stores: vec![MemoryStore::Semantic, MemoryStore::Episodic],
            strategy: RetrievalStrategy::Hybrid,
            allowed_shared_domains: Vec::new(),
        }
    }
}

#[derive(Clone)]
struct MemoryHydrator {
    manager: Arc<MemoryManager>,
    settings: MemoryHydrationSettings,
}

impl MaxOrchestrator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_tools(available_tools: Vec<ToolSpec>) -> Self {
        let resource_index = resource_index_from_tools(&available_tools);
        Self {
            available_tools,
            resource_index,
            routing_table: RoutingTable::default(),
            llm: None,
            memory_hydrator: None,
            skill_catalog: WorkspaceSkillCatalog::default(),
        }
    }

    pub fn with_resource_index(mut self, resource_index: ResourceIndex) -> Self {
        self.resource_index = resource_index.sorted();
        self
    }

    pub fn with_routing_table(mut self, routing_table: RoutingTable) -> Self {
        self.routing_table = routing_table;
        self
    }

    pub fn with_llm(mut self, llm: Arc<dyn Llm>) -> Self {
        self.llm = Some(llm);
        self
    }

    pub fn with_memory_hydrator(
        mut self,
        manager: Arc<MemoryManager>,
        settings: MemoryHydrationSettings,
    ) -> Self {
        self.memory_hydrator = Some(MemoryHydrator { manager, settings });
        self
    }

    pub fn with_skill_catalog(mut self, skill_catalog: WorkspaceSkillCatalog) -> Self {
        self.skill_catalog = skill_catalog;
        self
    }

    pub fn llm(&self) -> Option<&dyn Llm> {
        self.llm.as_deref()
    }

    pub fn available_tools(&self) -> &[ToolSpec] {
        &self.available_tools
    }

    pub fn memory_hydration_settings(&self) -> Option<&MemoryHydrationSettings> {
        self.memory_hydrator
            .as_ref()
            .map(|hydrator| &hydrator.settings)
    }

    pub fn resource_index(&self) -> &ResourceIndex {
        &self.resource_index
    }

    async fn plan_without_routing_fallback(
        &self,
        ctx: &RunContext<'_>,
    ) -> Result<Plan, OrchestratorError> {
        self.plan_internal(ctx, false).await
    }

    fn fallback_route(&self, ctx: &RunContext<'_>, input: &str) -> Option<Plan> {
        materialize_dispatch(ctx, input, &self.routing_table.fallback.dispatch)
    }

    async fn plan_with_llm(&self, ctx: &RunContext<'_>) -> Result<Plan, OrchestratorError> {
        let max_plan = self.plan_without_routing_fallback(ctx).await?;
        let Some(input) = latest_user_content(ctx) else {
            return Ok(max_plan);
        };
        if !should_route_to_llm(ctx, &max_plan) {
            return Ok(max_plan);
        }

        if let Some(plan) = self.route_user_text_with_llm(ctx, &input).await? {
            return Ok(plan);
        }
        if let Some(llm) = self.llm.as_ref().filter(|llm| llm.is_available()) {
            // Use the tool-aware path so the model can request `Plan::CallTool`
            // when it needs the filesystem / http / memory tools. The plain
            // `llm.complete(ctx)` default sends no `tools` field, so models
            // reply with "I can't modify files" even when tools are registered.
            let messages = ctx
                .state
                .transcript
                .items
                .iter()
                .map(|item| item.message.clone())
                .collect::<Vec<_>>();
            eprintln!(
                "MaxOrchestrator::plan_with_llm fallback: messages={} available_tools={}",
                messages.len(),
                self.available_tools.len()
            );
            let response = llm
                .complete_messages(&messages, &self.available_tools)
                .await
                .map_err(|err| OrchestratorError::Backend(Arc::from(err.to_string())))?;
            if let Some(first) = response.tool_calls.first().cloned() {
                return Ok(Plan::CallTool(first));
            }
            return Ok(Plan::Reply(response));
        }
        self.plan_internal(ctx, true).await
    }

    async fn route_user_text_with_llm(
        &self,
        ctx: &RunContext<'_>,
        input: &str,
    ) -> Result<Option<Plan>, OrchestratorError> {
        if self.routing_table.rules.is_empty() {
            return Ok(self.fallback_route(ctx, input));
        }
        let Some(llm) = self.llm.as_ref().filter(|llm| llm.is_available()) else {
            return Ok(None);
        };
        let Some(rule) = self.llm_route_rule(llm.as_ref(), input).await? else {
            return Ok(self.fallback_route(ctx, input));
        };
        Ok(materialize_dispatch(ctx, input, &rule.dispatch))
    }

    async fn llm_route_rule(
        &self,
        llm: &dyn Llm,
        input: &str,
    ) -> Result<Option<&RoutingRule>, OrchestratorError> {
        let messages = routing_classifier_messages(input, &self.routing_table);
        let response = llm
            .complete_messages(&messages, &[])
            .await
            .map_err(|err| OrchestratorError::Backend(Arc::from(err.to_string())))?;
        let Some(decision) = parse_routing_decision(&response.content) else {
            return Ok(None);
        };
        if decision.confidence < ROUTER_CONFIDENCE_THRESHOLD {
            return Ok(None);
        }
        Ok(rule_for_domain_key(&self.routing_table, &decision.domain))
    }
}

fn should_route_to_llm(ctx: &RunContext<'_>, plan: &Plan) -> bool {
    let Some(item) = ctx.state.transcript.items.last() else {
        return false;
    };
    if item.message.role != MessageRole::User {
        return false;
    }
    matches!(
        plan,
        Plan::Reply(message)
            if message.role == MessageRole::Assistant && message.content == item.message.content
    )
}

#[async_trait]
impl Orchestrator for MaxOrchestrator {
    async fn hydrate(&self, ctx: &mut RunContext<'_>) -> Result<(), OrchestratorError> {
        if ctx.resource_index.entries.is_empty() {
            ctx.resource_index = self.resource_index.clone();
        }
        let Some(hydrator) = &self.memory_hydrator else {
            return Ok(());
        };
        hydrator.hydrate(ctx).await?;
        Ok(())
    }

    async fn plan(&self, ctx: &RunContext<'_>) -> Result<Plan, OrchestratorError> {
        if self.llm.is_some() {
            return self.plan_with_llm(ctx).await;
        }
        self.plan_internal(ctx, true).await
    }
}

impl MemoryHydrator {
    async fn hydrate(&self, ctx: &mut RunContext<'_>) -> Result<(), OrchestratorError> {
        if !self.settings.enabled {
            return Ok(());
        }
        let Some(query) = latest_user_content(ctx) else {
            return Ok(());
        };
        if query.trim().is_empty() {
            return Ok(());
        }

        let caller = memory_caller_from_context(ctx, self.settings.allowed_shared_domains.clone());
        let result = self
            .manager
            .hydrate_with_stats(
                &caller,
                HydrationRequest {
                    query,
                    domain: None,
                    max_fragments: self.settings.max_fragments,
                    max_tokens: self.settings.max_estimated_tokens,
                    stores: self.settings.stores.clone(),
                    strategy: self.settings.strategy,
                },
            )
            .await
            .map_err(|err| OrchestratorError::Backend(Arc::from(err.to_string())))?;

        ctx.memory_fragments.extend(result.fragments);
        ctx.system.metadata.insert(
            Arc::from("memory_hydration_candidate_count"),
            serde_json::Value::from(result.stats.candidate_count),
        );
        ctx.system.metadata.insert(
            Arc::from("memory_hydration_selected_count"),
            serde_json::Value::from(result.stats.selected_count),
        );
        ctx.system.metadata.insert(
            Arc::from("memory_hydration_namespace_count"),
            serde_json::Value::from(result.stats.namespace_count),
        );
        Ok(())
    }
}

impl MaxOrchestrator {
    async fn plan_internal(
        &self,
        ctx: &RunContext<'_>,
        allow_routing_fallback: bool,
    ) -> Result<Plan, OrchestratorError> {
        if let Some(plan) = WebResearchSkill::new(&self.skill_catalog).plan(ctx)? {
            return Ok(plan);
        }

        let Some(item) = ctx.state.transcript.items.last() else {
            return Ok(Plan::Reply(Message::text(MessageRole::Assistant, "")));
        };

        match item.message.role {
            MessageRole::Tool => Ok(Plan::Reply(Message::text(
                MessageRole::Assistant,
                Arc::clone(&item.message.content),
            ))),
            MessageRole::User => {
                if let Some(plan) = deterministic_plan_from_user_text(&item.message.content)? {
                    return Ok(plan);
                }
                if let Some(plan) =
                    self.route_user_text(ctx, &item.message.content, allow_routing_fallback)
                {
                    return Ok(plan);
                }
                Ok(Plan::Reply(Message::text(
                    MessageRole::Assistant,
                    Arc::clone(&item.message.content),
                )))
            }
            MessageRole::Assistant | MessageRole::System => Ok(Plan::Reply(Message::text(
                MessageRole::Assistant,
                Arc::clone(&item.message.content),
            ))),
        }
    }

    fn route_user_text(
        &self,
        ctx: &RunContext<'_>,
        input: &str,
        allow_fallback: bool,
    ) -> Option<Plan> {
        let rule = classify_task(input)
            .and_then(|domain| rule_for_domain(&self.routing_table, &domain))
            .or_else(|| allow_fallback.then_some(&self.routing_table.fallback))?;
        materialize_dispatch(ctx, input, &rule.dispatch)
    }
}

fn resource_index_from_tools(tools: &[ToolSpec]) -> ResourceIndex {
    ResourceIndex {
        entries: tools
            .iter()
            .map(|tool| ResourceEntry {
                name: Arc::clone(&tool.name),
                kind: ResourceKind::Tool,
                summary: Arc::clone(&tool.description),
                priority: DispatchPriority::ToolOrMcp,
            })
            .collect(),
    }
    .sorted()
}
