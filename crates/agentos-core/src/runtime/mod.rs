use crate::approve::{Policy, PolicyAction, PolicyRule, PolicyVerb};
use crate::config::{SubAgentConfig, TemplateConfig, WorkspaceConfig};
use crate::guardrails::{MaxOutputLength, PiiFilter, ShellCommandAllowlist};
use crate::memory::{
    InMemoryMemory, MemoryManager, QdrantSemanticIndex, SqliteStore, SqliteVecSemanticIndex,
};
use crate::orchestrator::{EchoOrchestrator, MaxOrchestrator, MinOrchestrator};
use crate::r#loop::{InputGuardrailEntry, OutputGuardrailEntry, ToolGuardrailEntry};
use crate::runner::{JsonlTraceSink, RunnerDeps, TraceSink};
use crate::skills::WorkspaceSkillCatalog;
use crate::subagents::{SubAgentDefinition, SubAgentRegistry};
use crate::task_workspace::TaskWorkspace;
use crate::tools::{
    FileTool, HttpTool, MemoryTool, ShellTool, StaticMcpClient, StaticMcpTool, StdioMcpClient,
    ToolRegistry,
};
use agentos_interfaces::mcp::{McpClient, McpServer};
use agentos_interfaces::orchestrator::{Orchestrator, OrchestratorError, Plan, RunContext};
use agentos_interfaces::tool::ToolSpec;
use agentos_llm::{EnvLlm, LlmModelController, LlmModelTier};
use agentos_proto::AgentId;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("runtime failed: {0}")]
    Failed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimePaths {
    pub agent_config_path: PathBuf,
    pub session_db_path: PathBuf,
    pub trace_dir: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrchestratorStrategy {
    Max,
    Min,
}

impl OrchestratorStrategy {
    pub fn from_config(input: &str) -> Result<Self, String> {
        match input.trim().to_ascii_lowercase().as_str() {
            "builtin.max" | "max" | "builtin.tool_selecting" | "tool_selecting" => Ok(Self::Max),
            "builtin.min" | "min" | "builtin.llm" | "builtin.llm_fallback" | "llm" => Ok(Self::Min),
            other => Err(format!(
                "unknown orchestrator strategy '{other}'; expected builtin.max or builtin.min"
            )),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Max => "max",
            Self::Min => "min",
        }
    }

    pub fn task_id(self) -> &'static str {
        match self {
            Self::Max => "main",
            Self::Min => "min",
        }
    }
}

pub struct StrategyOrchestrator {
    strategy: Arc<Mutex<OrchestratorStrategy>>,
    max: MaxOrchestrator,
    min: MinOrchestrator,
}

impl StrategyOrchestrator {
    pub fn new(strategy: OrchestratorStrategy, max: MaxOrchestrator, min: MinOrchestrator) -> Self {
        Self {
            strategy: Arc::new(Mutex::new(strategy)),
            max,
            min,
        }
    }

    pub fn strategy_handle(&self) -> Arc<Mutex<OrchestratorStrategy>> {
        self.strategy.clone()
    }

    pub fn current_strategy(&self) -> OrchestratorStrategy {
        self.strategy
            .lock()
            .map(|guard| *guard)
            .unwrap_or(OrchestratorStrategy::Max)
    }

    pub fn describe_llm(&self) -> String {
        let llm = self
            .max
            .llm()
            .map(|llm| llm.describe())
            .unwrap_or_else(|| "llm provider=builtin.echo".to_owned());
        format!("orchestrator={}, {llm}", self.current_strategy().name())
    }

    pub fn memory_hydration_settings(
        &self,
    ) -> Option<&crate::orchestrator::MemoryHydrationSettings> {
        self.max.memory_hydration_settings()
    }
}

#[async_trait]
impl Orchestrator for StrategyOrchestrator {
    async fn hydrate(&self, ctx: &mut RunContext<'_>) -> Result<(), OrchestratorError> {
        match self.current_strategy() {
            OrchestratorStrategy::Max => self.max.hydrate(ctx).await,
            OrchestratorStrategy::Min => self.min.hydrate(ctx).await,
        }
    }

    async fn plan(&self, ctx: &RunContext<'_>) -> Result<Plan, OrchestratorError> {
        match self.current_strategy() {
            OrchestratorStrategy::Max => self.max.plan(ctx).await,
            OrchestratorStrategy::Min => self.min.plan(ctx).await,
        }
    }
}

pub struct AgentRuntime {
    pub workspace_config: WorkspaceConfig,
    pub session: Arc<SqliteStore>,
    pub memory_manager: Arc<MemoryManager>,
    pub model_controller: LlmModelController,
    pub orchestrator: StrategyOrchestrator,
    pub active_agent: AgentId,
    pub skill_catalog: WorkspaceSkillCatalog,
    tools: ToolRegistry,
    policy: Policy,
    subagents: Option<SubAgentRegistry>,
    trace_sink: Arc<dyn TraceSink>,
    task_workspace: Arc<TaskWorkspace>,
    pii_filter: PiiFilter,
    max_output_length: MaxOutputLength,
    shell_allowlist: ShellCommandAllowlist,
}

impl AgentRuntime {
    pub async fn build(paths: RuntimePaths) -> Result<Self, String> {
        let workspace_config = load_workspace_config(&paths.agent_config_path)
            .map_err(|err| format!("failed to load workspace config: {err}"))?;
        if workspace_config.memory.semantic_backend_is_sqlite_vec() {
            SqliteVecSemanticIndex::register_auto_extension()
                .map_err(|err| format!("failed to register sqlite-vec extension: {err}"))?;
        }
        let session = Arc::new(
            SqliteStore::open(paths.session_db_path)
                .map_err(|err| format!("failed to open session store: {err}"))?,
        );
        let memory_manager = build_memory_manager(&workspace_config, session.clone())?;
        let mut tools = ToolRegistry::reference_with_memory_manager(memory_manager.clone());
        if let Some(path) = isolation_worker_path(&workspace_config) {
            tools = tools.with_subprocess_isolation(path);
        }
        let mcp_specs = register_configured_mcp(&mut tools, &workspace_config).await?;
        let model_controller = LlmModelController::new();
        let subagents = build_subagents(
            &workspace_config,
            model_controller.clone(),
            memory_manager.clone(),
        )?;
        let skill_catalog = WorkspaceSkillCatalog::load_enabled(
            &skills_root(&paths.agent_config_path),
            &workspace_config.resources.skills.enabled,
        )
        .map_err(|err| format!("failed to load workspace skills: {err}"))?;
        let resource_index =
            workspace_config.resource_index(&tools.specs(), &mcp_specs, &skill_catalog.metadata());
        let routing_table = workspace_config.routing_table()?;
        let high_llm = Arc::new(EnvLlm::new(LlmModelTier::High, model_controller.clone())?);
        let max_orchestrator = MaxOrchestrator::with_tools(tools.specs())
            .with_resource_index(resource_index)
            .with_routing_table(routing_table)
            .with_skill_catalog(skill_catalog.clone())
            .with_llm(high_llm.clone())
            .with_memory_hydrator(
                memory_manager.clone(),
                workspace_config.memory.hydration_settings()?,
            );
        let min_orchestrator = MinOrchestrator::new(high_llm);
        let orchestrator_strategy =
            OrchestratorStrategy::from_config(&workspace_config.agent.orchestrator)?;
        let orchestrator =
            StrategyOrchestrator::new(orchestrator_strategy, max_orchestrator, min_orchestrator);
        let policy = phase5_policy(&workspace_config, &mcp_specs);
        let trace_sink: Arc<dyn TraceSink> = Arc::new(JsonlTraceSink::new(paths.trace_dir));
        let task_workspace = Arc::new(TaskWorkspace::new(
            workspace_config.task_workspace.root.clone(),
        ));
        let subagents = subagents.map(|registry| {
            registry
                .with_trace_sink(trace_sink.clone())
                .with_task_workspace(task_workspace.clone())
        });

        Ok(Self {
            workspace_config,
            session,
            memory_manager,
            model_controller,
            orchestrator,
            active_agent: AgentId::new("main-agent"),
            skill_catalog,
            tools,
            policy,
            subagents,
            trace_sink,
            task_workspace,
            pii_filter: PiiFilter,
            max_output_length: MaxOutputLength::new(8_000),
            shell_allowlist: ShellCommandAllowlist::new(["printf", "echo", "pwd", "ls"]),
        })
    }

    pub fn deps_scope(&self) -> RuntimeDepsScope<'_> {
        RuntimeDepsScope { runtime: self }
    }
}

pub fn skills_root(agent_config_path: &Path) -> PathBuf {
    agent_config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("skills")
}

pub fn load_workspace_config(path: &Path) -> Result<WorkspaceConfig, std::io::Error> {
    let config_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut config = match std::fs::read_to_string(path) {
        Ok(input) => toml::from_str(&input).map_err(std::io::Error::other)?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => WorkspaceConfig::default(),
        Err(err) => return Err(err),
    };
    resolve_workspace_paths(&mut config, config_dir);
    config.validate_memory().map_err(std::io::Error::other)?;
    config.subagents.extend(load_subagent_files(config_dir)?);
    config
        .orchestrator_templates
        .extend(load_suborch_files(config_dir)?);
    config.validate_subagents().map_err(std::io::Error::other)?;
    config.routing_table().map_err(std::io::Error::other)?;
    Ok(config)
}

fn resolve_workspace_paths(config: &mut WorkspaceConfig, config_dir: &Path) {
    if let Some(path) = &config.memory.path {
        if path.is_relative() {
            config.memory.path = Some(config_dir.join(path));
        }
    }
    if config.task_workspace.root.is_relative() {
        config.task_workspace.root = config_dir.join(&config.task_workspace.root);
    }
    if let Some(worker_path) = &config.isolation.worker_path {
        if worker_path.is_relative() {
            config.isolation.worker_path = Some(config_dir.join(worker_path));
        }
    }
}

fn load_subagent_files(config_dir: &Path) -> Result<Vec<SubAgentConfig>, std::io::Error> {
    let mut files = workspace_toml_files(&config_dir.join("subagents"))?;
    files
        .drain(..)
        .map(|path| {
            let input = std::fs::read_to_string(&path)?;
            let mut subagent: SubAgentConfig =
                toml::from_str(&input).map_err(std::io::Error::other)?;
            if subagent.name.is_empty() {
                subagent.name = Arc::clone(&subagent.id);
            }
            Ok(subagent)
        })
        .collect()
}

fn load_suborch_files(config_dir: &Path) -> Result<Vec<TemplateConfig>, std::io::Error> {
    let mut files = workspace_toml_files(&config_dir.join("suborchs"))?;
    files
        .drain(..)
        .map(|path| {
            let input = std::fs::read_to_string(&path)?;
            toml::from_str(&input).map_err(std::io::Error::other)
        })
        .collect()
}

fn workspace_toml_files(root: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut files = entries
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

fn build_memory_manager(
    config: &WorkspaceConfig,
    session: Arc<SqliteStore>,
) -> Result<Arc<MemoryManager>, String> {
    let (manager, sqlite_store) = if config.memory.backend_is_in_memory() {
        (
            MemoryManager::new(Arc::new(InMemoryMemory::default())),
            None,
        )
    } else if let Some(path) = &config.memory.path {
        let store = Arc::new(SqliteStore::open(path).map_err(|err| {
            format!(
                "failed to open configured memory store '{}': {err}",
                path.display()
            )
        })?);
        (MemoryManager::new_sqlite(store.clone()), Some(store))
    } else {
        (MemoryManager::new_sqlite(session.clone()), Some(session))
    };

    if config.memory.semantic_backend_is_qdrant() {
        let qdrant = QdrantSemanticIndex::new((&config.memory.qdrant).into())
            .map_err(|err| format!("failed to configure qdrant semantic memory: {err}"))?;
        return Ok(Arc::new(manager.with_semantic_index(Arc::new(qdrant))));
    }

    if config.memory.semantic_backend_is_sqlite_vec() {
        let Some(sqlite_store) = sqlite_store else {
            return Err("sqlite_vec semantic memory requires a sqlite memory backend".to_owned());
        };
        let sqlite_vec =
            SqliteVecSemanticIndex::new(sqlite_store, (&config.memory.sqlite_vec).into())
                .map_err(|err| format!("failed to configure sqlite_vec semantic memory: {err}"))?;
        return Ok(Arc::new(manager.with_semantic_index(Arc::new(sqlite_vec))));
    }

    Ok(Arc::new(manager))
}

pub struct RuntimeDepsScope<'a> {
    runtime: &'a AgentRuntime,
}

impl<'a> RuntimeDepsScope<'a> {
    pub fn deps(&'a self) -> RunnerDeps<'a> {
        RunnerDeps {
            orchestrator: &self.runtime.orchestrator,
            session: self.runtime.session.as_ref(),
            memory_manager: self.episode_memory_manager(),
            hooks: None,
            max_turns: 4,
            active_agent: self.runtime.active_agent.clone(),
            tools: Some(&self.runtime.tools),
            trace_sink: Some(self.runtime.trace_sink.as_ref()),
            task_workspace: Some(self.runtime.task_workspace.as_ref()),
            policy: &self.runtime.policy,
            subagents: self.runtime.subagents.as_ref(),
            input_guardrails: &[],
            output_guardrails: &[],
            tool_guardrails: &[],
        }
    }

    pub fn input_guardrails(&'a self) -> [InputGuardrailEntry<'a>; 1] {
        [InputGuardrailEntry {
            name: Arc::from("PiiFilter"),
            guardrail: &self.runtime.pii_filter,
        }]
    }

    pub fn output_guardrails(&'a self) -> [OutputGuardrailEntry<'a>; 1] {
        [OutputGuardrailEntry {
            name: Arc::from("MaxOutputLength"),
            guardrail: &self.runtime.max_output_length,
        }]
    }

    pub fn tool_guardrails(&'a self) -> [ToolGuardrailEntry<'a>; 1] {
        [ToolGuardrailEntry {
            name: Arc::from("ShellCommandAllowlist"),
            guardrail: &self.runtime.shell_allowlist,
        }]
    }

    pub fn deps_with_guardrails(
        &'a self,
        input_guardrails: &'a [InputGuardrailEntry<'a>],
        output_guardrails: &'a [OutputGuardrailEntry<'a>],
        tool_guardrails: &'a [ToolGuardrailEntry<'a>],
    ) -> RunnerDeps<'a> {
        RunnerDeps {
            orchestrator: &self.runtime.orchestrator,
            session: self.runtime.session.as_ref(),
            memory_manager: self.episode_memory_manager(),
            hooks: None,
            max_turns: 4,
            active_agent: self.runtime.active_agent.clone(),
            tools: Some(&self.runtime.tools),
            trace_sink: Some(self.runtime.trace_sink.as_ref()),
            task_workspace: Some(self.runtime.task_workspace.as_ref()),
            policy: &self.runtime.policy,
            subagents: self.runtime.subagents.as_ref(),
            input_guardrails,
            output_guardrails,
            tool_guardrails,
        }
    }

    fn episode_memory_manager(&'a self) -> Option<&'a MemoryManager> {
        self.runtime
            .workspace_config
            .memory
            .episode_recording_enabled
            .then_some(self.runtime.memory_manager.as_ref())
    }
}

pub fn build_subagents(
    config: &WorkspaceConfig,
    models: LlmModelController,
    memory_manager: Arc<MemoryManager>,
) -> Result<Option<SubAgentRegistry>, String> {
    if config.subagents.is_empty() {
        return Ok(None);
    }

    let mut registry = SubAgentRegistry::new();
    for subagent in &config.subagents {
        let mut tools = ToolRegistry::new();
        for tool in &subagent.tools {
            if tool.as_ref() != "memory" {
                register_builtin_tool(&mut tools, tool);
            }
        }
        if subagent_memory_tool_enabled(subagent) {
            tools.register(MemoryTool::with_manager(memory_manager.clone()));
        }
        let mut definition = SubAgentDefinition::new(
            AgentId::new(Arc::clone(&subagent.id)),
            Arc::clone(&subagent.policy_id),
            subagent_orchestrator(subagent, models.clone())?,
            subagent_policy(subagent)?,
        )
        .with_tools(Arc::new(tools))
        .with_max_turns(subagent.max_turns);
        if subagent.memory_view.as_ref() != "none" || subagent_memory_tool_enabled(subagent) {
            definition = definition.with_memory_manager(memory_manager.clone());
        }
        if subagent.inherit_guardrails {
            definition = definition
                .with_input_guardrail("PiiFilter", PiiFilter)
                .with_output_guardrail("MaxOutputLength", MaxOutputLength::new(8_000))
                .with_tool_guardrail(
                    "ShellCommandAllowlist",
                    ShellCommandAllowlist::new(["printf", "echo", "pwd", "ls"]),
                );
        }
        registry.register(definition);
    }
    Ok(Some(registry))
}

pub fn isolation_worker_path(config: &WorkspaceConfig) -> Option<PathBuf> {
    env::var_os("AGENTOS_TOOL_WORKER_PATH")
        .map(PathBuf::from)
        .or_else(|| {
            config
                .isolation
                .worker_path_env
                .as_deref()
                .and_then(env::var_os)
                .map(PathBuf::from)
        })
        .or_else(|| config.isolation.worker_path.clone())
}

pub async fn register_configured_mcp(
    tools: &mut ToolRegistry,
    config: &WorkspaceConfig,
) -> Result<Vec<ToolSpec>, String> {
    if config.mcp_servers.is_empty() {
        return Ok(Vec::new());
    }

    let static_client = Arc::new(StaticMcpClient::new(config.mcp_tools.iter().map(|tool| {
        StaticMcpTool {
            server_id: Arc::clone(&tool.server_id),
            spec: ToolSpec {
                name: Arc::clone(&tool.name),
                description: Arc::clone(&tool.description),
                input_schema: serde_json::json!({ "type": "object" }),
                requires_isolation: tool.requires_isolation,
            },
            response: Arc::clone(&tool.response),
        }
    })));

    let mut specs = Vec::new();
    for server in &config.mcp_servers {
        let client: Arc<dyn McpClient> =
            if server.endpoint.starts_with("stdio://") || server.endpoint.starts_with("stdio:") {
                let timeout = server
                    .timeout_ms
                    .map(Duration::from_millis)
                    .unwrap_or_else(|| Duration::from_secs(10));
                Arc::new(StdioMcpClient::with_timeout(timeout))
            } else if config
                .mcp_tools
                .iter()
                .any(|tool| tool.server_id == server.id)
                || server.endpoint.starts_with("static://")
            {
                static_client.clone()
            } else {
                return Err(format!("unsupported MCP endpoint: {}", server.endpoint));
            };
        specs.extend(
            tools
                .register_mcp_server(
                    McpServer {
                        id: Arc::clone(&server.id),
                        endpoint: Arc::clone(&server.endpoint),
                    },
                    client,
                )
                .await
                .map_err(|err| format!("failed to register MCP server {}: {err}", server.id))?,
        );
    }
    Ok(specs)
}

pub fn phase5_policy(config: &WorkspaceConfig, mcp_specs: &[ToolSpec]) -> Policy {
    let mut policy = Policy::phase4_reference();
    if !config.subagents.is_empty() {
        policy.rules.push(PolicyRule {
            action: PolicyAction::Delegate,
            decision: PolicyVerb::Allow,
            reason: None,
            arg_equals: BTreeMap::new(),
        });
    }
    if !config.orchestrator_templates.is_empty() {
        policy.rules.push(PolicyRule {
            action: PolicyAction::Escalate,
            decision: PolicyVerb::Allow,
            reason: None,
            arg_equals: BTreeMap::new(),
        });
    }
    for subagent in &config.subagents {
        for tool in &subagent.tools {
            allow_tool_once(&mut policy, Arc::clone(tool));
        }
    }
    for spec in mcp_specs {
        allow_tool_once(&mut policy, Arc::clone(&spec.name));
    }
    policy
}

fn subagent_policy(subagent: &SubAgentConfig) -> Result<Policy, String> {
    let mut policy = Policy::allow_tools(
        subagent
            .tools
            .iter()
            .filter(|tool| tool.as_ref() != "memory")
            .cloned(),
    );
    if subagent_memory_tool_enabled(subagent) {
        for operation in subagent_memory_operations(subagent)? {
            let (decision, reason) = match operation.as_ref() {
                "read" => (PolicyVerb::Allow, None),
                "write" => (
                    PolicyVerb::AskUser,
                    Some(Arc::from("memory write requires user approval")),
                ),
                "forget" => (
                    PolicyVerb::AskUser,
                    Some(Arc::from("memory forget requires user approval")),
                ),
                other => {
                    return Err(format!(
                        "unknown subagent memory operation '{other}'; expected read, write, or forget"
                    ));
                }
            };
            policy.rules.push(PolicyRule {
                action: PolicyAction::Tool(Arc::from("memory")),
                decision,
                reason,
                arg_equals: BTreeMap::from([(
                    Arc::from("operation"),
                    Value::from(operation.as_ref()),
                )]),
            });
        }
    }
    Ok(policy)
}

fn subagent_memory_tool_enabled(subagent: &SubAgentConfig) -> bool {
    subagent.tools.iter().any(|tool| tool.as_ref() == "memory") || !subagent.memory_tools.is_empty()
}

fn subagent_memory_operations(subagent: &SubAgentConfig) -> Result<Vec<Arc<str>>, String> {
    if subagent.memory_tools.is_empty() {
        return Ok(vec![Arc::from("read"), Arc::from("write")]);
    }
    subagent
        .memory_tools
        .iter()
        .map(|operation| match operation.as_ref() {
            "read" | "write" | "forget" => Ok(Arc::clone(operation)),
            other => Err(format!(
                "unknown subagent memory operation '{other}'; expected read, write, or forget"
            )),
        })
        .collect()
}

pub fn register_builtin_tool(tools: &mut ToolRegistry, name: &str) {
    match name {
        "shell" => tools.register(ShellTool),
        "http" => tools.register(HttpTool),
        "file" => tools.register(FileTool),
        _ => {}
    }
}

fn subagent_orchestrator(
    subagent: &SubAgentConfig,
    models: LlmModelController,
) -> Result<Arc<dyn Orchestrator>, String> {
    let tier = LlmModelTier::from_config(&subagent.model_tier)?;
    match subagent.orchestrator.as_ref() {
        "builtin.echo" => Ok(Arc::new(EchoOrchestrator)),
        "builtin.min" | "builtin.llm" | "builtin.llm_fallback" => Ok(Arc::new(
            MinOrchestrator::new(Arc::new(EnvLlm::new(tier, models)?)),
        )),
        "builtin.max" | "builtin.tool_selecting" => Ok(Arc::new(
            MaxOrchestrator::new().with_llm(Arc::new(EnvLlm::new(tier, models)?)),
        )),
        _ => Ok(Arc::new(
            MaxOrchestrator::new().with_llm(Arc::new(EnvLlm::new(tier, models)?)),
        )),
    }
}

fn allow_tool_once(policy: &mut Policy, tool: Arc<str>) {
    if tool.as_ref() == "memory" {
        return;
    }
    if !policy.rules.iter().any(|rule| {
        rule.action == PolicyAction::Tool(Arc::clone(&tool))
            && rule.decision == PolicyVerb::Allow
            && rule.arg_equals.is_empty()
    }) {
        policy.rules.push(PolicyRule {
            action: PolicyAction::Tool(tool),
            decision: PolicyVerb::Allow,
            reason: None,
            arg_equals: BTreeMap::new(),
        });
    }
}

