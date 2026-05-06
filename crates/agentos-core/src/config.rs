use crate::memory::{MemoryStore, QdrantSemanticConfig, RetrievalStrategy, SqliteVecConfig};
use crate::orchestrator::MemoryHydrationSettings;
use crate::skills::WorkspaceSkillMetadata;
use agentos_interfaces::orchestrator::{
    DispatchPriority, DispatchTarget, OrchestratorTemplate, ResourceEntry, ResourceIndex,
    ResourceKind, RoutingRule, RoutingTable, Stage, SubAgentSpec, TaskDomain,
};
use agentos_interfaces::tool::ToolSpec;
use agentos_proto::AgentId;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct WorkspaceConfig {
    pub agent: AgentConfig,
    pub memory: MemoryConfig,
    pub isolation: IsolationConfig,
    pub subagents: Vec<SubAgentConfig>,
    pub mcp_servers: Vec<McpServerConfig>,
    pub mcp_tools: Vec<McpToolConfig>,
    pub resources: ResourceConfig,
    pub routing: RoutingConfig,
    pub orchestrator_templates: Vec<TemplateConfig>,
    pub task_workspace: TaskWorkspaceConfig,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct MemoryConfig {
    pub backend: Arc<str>,
    pub path: Option<PathBuf>,
    pub default_domain: Arc<str>,
    pub hydration_enabled: bool,
    pub hydrate_strategy: Arc<str>,
    pub hydrate_max_fragments: usize,
    pub hydrate_max_estimated_tokens: usize,
    pub hydrate_stores: Vec<Arc<str>>,
    pub semantic_backend: Arc<str>,
    pub qdrant: MemoryQdrantConfig,
    pub sqlite_vec: MemorySqliteVecConfig,
    pub episode_recording_enabled: bool,
    pub retention: MemoryRetentionConfig,
    pub policy: MemoryPolicyConfig,
    pub shared_domains: Vec<MemorySharedDomainConfig>,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            backend: Arc::from("sqlite"),
            path: None,
            default_domain: Arc::from("general"),
            hydration_enabled: false,
            hydrate_strategy: Arc::from("hybrid"),
            hydrate_max_fragments: 5,
            hydrate_max_estimated_tokens: 1_200,
            hydrate_stores: vec![Arc::from("semantic"), Arc::from("episodic")],
            semantic_backend: Arc::from("none"),
            qdrant: MemoryQdrantConfig::default(),
            sqlite_vec: MemorySqliteVecConfig::default(),
            episode_recording_enabled: false,
            retention: MemoryRetentionConfig::default(),
            policy: MemoryPolicyConfig::default(),
            shared_domains: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct MemorySqliteVecConfig {
    pub table: Arc<str>,
    pub vector_dimensions: usize,
}

impl Default for MemorySqliteVecConfig {
    fn default() -> Self {
        let defaults = SqliteVecConfig::default();
        Self {
            table: defaults.table,
            vector_dimensions: defaults.vector_dimensions,
        }
    }
}

impl From<&MemorySqliteVecConfig> for SqliteVecConfig {
    fn from(config: &MemorySqliteVecConfig) -> Self {
        Self {
            table: Arc::clone(&config.table),
            vector_dimensions: config.vector_dimensions,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct MemoryQdrantConfig {
    pub url: Arc<str>,
    pub collection: Arc<str>,
    pub vector_name: Option<Arc<str>>,
    pub vector_dimensions: usize,
    pub api_key: Option<Arc<str>>,
    pub timeout_ms: u64,
}

impl Default for MemoryQdrantConfig {
    fn default() -> Self {
        let defaults = QdrantSemanticConfig::default();
        Self {
            url: defaults.url,
            collection: defaults.collection,
            vector_name: defaults.vector_name,
            vector_dimensions: defaults.vector_dimensions,
            api_key: defaults.api_key,
            timeout_ms: defaults.timeout_ms,
        }
    }
}

impl From<&MemoryQdrantConfig> for QdrantSemanticConfig {
    fn from(config: &MemoryQdrantConfig) -> Self {
        Self {
            url: Arc::clone(&config.url),
            collection: Arc::clone(&config.collection),
            vector_name: config.vector_name.clone(),
            vector_dimensions: config.vector_dimensions,
            api_key: config.api_key.clone(),
            timeout_ms: config.timeout_ms,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct MemoryRetentionConfig {
    pub max_records: Option<usize>,
    pub max_bytes: Option<usize>,
    pub max_age_days: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct MemoryPolicyConfig {
    pub writes: Arc<str>,
    pub forgets: Arc<str>,
    pub shared_writes: bool,
}

impl Default for MemoryPolicyConfig {
    fn default() -> Self {
        Self {
            writes: Arc::from("ask_user"),
            forgets: Arc::from("ask_user"),
            shared_writes: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct MemorySharedDomainConfig {
    pub name: Arc<str>,
    pub read: bool,
    pub write: bool,
}

impl Default for MemorySharedDomainConfig {
    fn default() -> Self {
        Self {
            name: Arc::from("general"),
            read: true,
            write: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct AgentConfig {
    pub id: Arc<str>,
    pub orchestrator: Arc<str>,
    pub memory: Arc<str>,
    pub max_turns: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            id: Arc::from("default"),
            orchestrator: Arc::from("builtin.max"),
            memory: Arc::from("memory.in_memory"),
            max_turns: 16,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct IsolationConfig {
    pub worker_path: Option<PathBuf>,
    pub worker_path_env: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct McpServerConfig {
    pub id: Arc<str>,
    pub endpoint: Arc<str>,
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct McpToolConfig {
    pub server_id: Arc<str>,
    pub name: Arc<str>,
    pub description: Arc<str>,
    pub response: Arc<str>,
    pub requires_isolation: bool,
}

impl Default for McpToolConfig {
    fn default() -> Self {
        Self {
            server_id: Arc::from("static-mcp"),
            name: Arc::from("remote_echo"),
            description: Arc::from("Static MCP-backed tool"),
            response: Arc::from("static MCP response"),
            requires_isolation: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct SubAgentConfig {
    pub name: Arc<str>,
    pub id: Arc<str>,
    pub description: Arc<str>,
    pub developer_instructions: Arc<str>,
    pub policy_id: Arc<str>,
    pub orchestrator: Arc<str>,
    pub model_tier: Arc<str>,
    pub tools: Vec<Arc<str>>,
    pub memory_view: Arc<str>,
    pub memory_domains: Vec<Arc<str>>,
    pub memory_tools: Vec<Arc<str>>,
    pub max_turns: usize,
    pub inherit_guardrails: bool,
}

impl Default for SubAgentConfig {
    fn default() -> Self {
        Self {
            name: Arc::from("research-subagent"),
            id: Arc::from("research-subagent"),
            description: Arc::from(""),
            developer_instructions: Arc::from(""),
            policy_id: Arc::from("readonly-web"),
            orchestrator: Arc::from("builtin.max"),
            model_tier: Arc::from("medium"),
            tools: vec![Arc::from("http")],
            memory_view: Arc::from("none"),
            memory_domains: Vec::new(),
            memory_tools: Vec::new(),
            max_turns: 4,
            inherit_guardrails: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct ResourceConfig {
    pub priority: Vec<Arc<str>>,
    pub skills: ResourceSection,
    pub tools: ResourceSection,
    pub mcp: ResourceSection,
    pub llm: ResourceSection,
}

impl Default for ResourceConfig {
    fn default() -> Self {
        Self {
            priority: vec![
                Arc::from("skills"),
                Arc::from("tools"),
                Arc::from("mcp"),
                Arc::from("llm"),
            ],
            skills: ResourceSection::default(),
            tools: ResourceSection::default(),
            mcp: ResourceSection::default(),
            llm: ResourceSection::default(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct ResourceSection {
    pub enabled: Vec<Arc<str>>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct RoutingConfig {
    pub rules: Vec<RoutingRuleConfig>,
    pub fallback: Option<RoutingRuleConfig>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct RoutingRuleConfig {
    pub domain: Arc<str>,
    pub description: Arc<str>,
    pub examples: Vec<Arc<str>>,
    pub dispatch: Arc<str>,
    pub template: Option<Arc<str>>,
    pub agent_id: Option<Arc<str>>,
    pub policy_id: Option<Arc<str>>,
}

impl Default for RoutingRuleConfig {
    fn default() -> Self {
        Self {
            domain: Arc::from("general"),
            description: Arc::from(""),
            examples: Vec::new(),
            dispatch: Arc::from("direct"),
            template: None,
            agent_id: None,
            policy_id: None,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct TemplateConfig {
    pub name: Arc<str>,
    pub description: Arc<str>,
    pub developer_instructions: Arc<str>,
    pub stages: Vec<StageConfig>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct StageConfig {
    pub name: Arc<str>,
    pub agent_id: Arc<str>,
    pub policy_id: Arc<str>,
    pub depends_on: Vec<Arc<str>>,
}

impl Default for StageConfig {
    fn default() -> Self {
        Self {
            name: Arc::from("stage"),
            agent_id: Arc::from("general-subagent"),
            policy_id: Arc::from("default"),
            depends_on: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct TaskWorkspaceConfig {
    pub root: PathBuf,
}

impl Default for TaskWorkspaceConfig {
    fn default() -> Self {
        Self {
            root: PathBuf::from("tasks"),
        }
    }
}

impl WorkspaceConfig {
    pub fn load(path: &Path) -> Result<Self, std::io::Error> {
        let config_dir = path.parent().unwrap_or_else(|| Path::new("."));
        let mut config = match std::fs::read_to_string(path) {
            Ok(input) => toml::from_str(&input).map_err(std::io::Error::other)?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => return Err(err),
        };
        config.resolve_paths(config_dir);
        config.validate_memory().map_err(std::io::Error::other)?;
        config.validate_subagents().map_err(std::io::Error::other)?;
        config.routing_table().map_err(std::io::Error::other)?;
        Ok(config)
    }

    fn resolve_paths(&mut self, config_dir: &Path) {
        if let Some(path) = &self.memory.path {
            if path.is_relative() {
                self.memory.path = Some(config_dir.join(path));
            }
        }
        if self.task_workspace.root.is_relative() {
            self.task_workspace.root = config_dir.join(&self.task_workspace.root);
        }
        if let Some(worker_path) = &self.isolation.worker_path {
            if worker_path.is_relative() {
                self.isolation.worker_path = Some(config_dir.join(worker_path));
            }
        }
    }

    pub fn validate_memory(&mut self) -> Result<(), String> {
        self.memory.validate()
    }

    pub fn validate_subagents(&mut self) -> Result<(), String> {
        for subagent in &mut self.subagents {
            subagent.memory_view = Arc::from(normalize_memory_view(&subagent.memory_view)?);
            for domain in &mut subagent.memory_domains {
                *domain = normalize_domain(domain, "subagents.memory_domains")?;
            }
            for tool in &mut subagent.memory_tools {
                *tool = Arc::from(normalize_memory_tool(tool)?);
            }
            if subagent.memory_view.as_ref() == "none" && !subagent.memory_domains.is_empty() {
                return Err(format!(
                    "subagent '{}' sets memory_domains without enabling memory_view",
                    subagent.id
                ));
            }
        }
        Ok(())
    }

    fn subagent_metadata(
        &self,
        agent_id: &Arc<str>,
        policy_id: &Arc<str>,
    ) -> Result<BTreeMap<Arc<str>, serde_json::Value>, String> {
        let subagent = self
            .subagents
            .iter()
            .find(|subagent| subagent.id == *agent_id && subagent.policy_id == *policy_id)
            .ok_or_else(|| {
                format!(
                    "unknown subagent '{}' with policy '{}'",
                    agent_id, policy_id
                )
            })?;
        subagent_memory_metadata(subagent)
    }

    pub fn resource_index(
        &self,
        tool_specs: &[ToolSpec],
        mcp_specs: &[ToolSpec],
        skill_specs: &[WorkspaceSkillMetadata],
    ) -> ResourceIndex {
        let mcp_names = mcp_specs
            .iter()
            .map(|spec| Arc::clone(&spec.name))
            .collect::<BTreeSet<_>>();
        let tools = tool_specs
            .iter()
            .filter(|spec| !mcp_names.contains(&spec.name))
            .collect::<Vec<_>>();
        let mcp = mcp_specs.iter().collect::<Vec<_>>();
        let mut entries = Vec::new();

        for priority in &self.resources.priority {
            match priority.as_ref() {
                "skills" => {
                    for skill in skill_specs {
                        entries.push(ResourceEntry {
                            name: Arc::clone(&skill.name),
                            kind: ResourceKind::Skill,
                            summary: Arc::clone(&skill.description),
                            priority: DispatchPriority::Skill,
                        });
                    }
                }
                "tools" => {
                    for tool in &tools {
                        entries.push(ResourceEntry {
                            name: Arc::clone(&tool.name),
                            kind: ResourceKind::Tool,
                            summary: Arc::clone(&tool.description),
                            priority: DispatchPriority::ToolOrMcp,
                        });
                    }
                }
                "mcp" => {
                    for tool in &mcp {
                        entries.push(ResourceEntry {
                            name: Arc::clone(&tool.name),
                            kind: ResourceKind::Mcp,
                            summary: Arc::clone(&tool.description),
                            priority: DispatchPriority::ToolOrMcp,
                        });
                    }
                }
                "llm" => {
                    if self.resources.llm.enabled.is_empty() {
                        entries.push(ResourceEntry {
                            name: Arc::from("llm"),
                            kind: ResourceKind::Llm,
                            summary: Arc::from("Fallback language model reasoning"),
                            priority: DispatchPriority::LlmFallback,
                        });
                    } else {
                        for llm in &self.resources.llm.enabled {
                            entries.push(ResourceEntry {
                                name: Arc::clone(llm),
                                kind: ResourceKind::Llm,
                                summary: Arc::from("Configured language model fallback"),
                                priority: DispatchPriority::LlmFallback,
                            });
                        }
                    }
                }
                _ => {}
            }
        }

        ResourceIndex { entries }.sorted()
    }

    pub fn routing_table(&self) -> Result<RoutingTable, String> {
        self.validate_template_references()?;
        let templates = self.template_map()?;
        let rules = self
            .routing
            .rules
            .iter()
            .map(|rule| -> Result<RoutingRule, String> {
                Ok(RoutingRule {
                    domain: parse_domain(&rule.domain),
                    description: Arc::clone(&rule.description),
                    examples: rule.examples.clone(),
                    dispatch: parse_dispatch(rule, &templates, self)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let fallback = self.routing.fallback.as_ref().map_or_else(
            || -> Result<RoutingRule, String> {
                Ok(RoutingRule {
                    domain: TaskDomain::General,
                    description: Arc::from("General-purpose fallback for unclassified prompts."),
                    examples: Vec::new(),
                    dispatch: DispatchTarget::Direct,
                })
            },
            |rule| -> Result<RoutingRule, String> {
                Ok(RoutingRule {
                    domain: parse_domain(&rule.domain),
                    description: Arc::clone(&rule.description),
                    examples: rule.examples.clone(),
                    dispatch: parse_dispatch(rule, &templates, self)?,
                })
            },
        )?;
        Ok(RoutingTable { rules, fallback })
    }

    fn template_map(&self) -> Result<BTreeMap<Arc<str>, OrchestratorTemplate>, String> {
        self.orchestrator_templates
            .iter()
            .map(|template| {
                template
                    .to_template(self)
                    .map(|value| (Arc::clone(&template.name), value))
            })
            .collect()
    }

    fn validate_template_references(&self) -> Result<(), String> {
        for template in &self.orchestrator_templates {
            for stage in &template.stages {
                if !self.subagents.iter().any(|subagent| {
                    subagent.id == stage.agent_id && subagent.policy_id == stage.policy_id
                }) {
                    return Err(format!(
                        "template '{}' stage '{}' references unknown subagent '{}' with policy '{}'",
                        template.name, stage.name, stage.agent_id, stage.policy_id
                    ));
                }
            }
        }
        Ok(())
    }
}

impl MemoryConfig {
    pub fn validate(&mut self) -> Result<(), String> {
        self.backend = Arc::from(normalize_config_token(&self.backend));
        match self.backend.as_ref() {
            "sqlite" | "memory.sqlite" | "in_memory" | "memory.in_memory" => {}
            other => {
                return Err(format!(
                    "unknown memory backend '{other}'; expected sqlite or in_memory"
                ));
            }
        }
        self.hydrate_strategy = Arc::from(normalize_config_token(&self.hydrate_strategy));
        parse_retrieval_strategy(&self.hydrate_strategy)?;
        self.semantic_backend = Arc::from(normalize_config_token(&self.semantic_backend));
        match self.semantic_backend.as_ref() {
            "none" | "qdrant" | "memory.qdrant" | "sqlite" | "memory.sqlite" | "sqlite_vec"
            | "memory.sqlite_vec" => {}
            other => {
                return Err(format!(
                    "unknown memory semantic_backend '{other}'; expected none, sqlite/sqlite_vec, or qdrant"
                ));
            }
        }
        validate_qdrant_config(&self.qdrant)?;
        validate_sqlite_vec_config(&self.sqlite_vec)?;

        self.default_domain = normalize_domain(&self.default_domain, "memory.default_domain")?;
        if self.hydrate_max_fragments == 0 {
            return Err("memory.hydrate_max_fragments must be greater than 0".to_owned());
        }
        if self.hydrate_max_estimated_tokens == 0 {
            return Err("memory.hydrate_max_estimated_tokens must be greater than 0".to_owned());
        }
        if self.hydrate_stores.is_empty() {
            return Err("memory.hydrate_stores must include at least one store".to_owned());
        }
        for store in &self.hydrate_stores {
            parse_memory_store(store)?;
        }
        validate_optional_budget(self.retention.max_records, "memory.retention.max_records")?;
        validate_optional_budget(self.retention.max_bytes, "memory.retention.max_bytes")?;
        if self.retention.max_age_days == Some(0) {
            return Err("memory.retention.max_age_days must be greater than 0".to_owned());
        }
        self.policy.writes = Arc::from(normalize_config_token(&self.policy.writes));
        self.policy.forgets = Arc::from(normalize_config_token(&self.policy.forgets));
        validate_memory_policy(&self.policy.writes, "memory.policy.writes")?;
        validate_memory_policy(&self.policy.forgets, "memory.policy.forgets")?;
        for domain in &mut self.shared_domains {
            domain.name = normalize_domain(&domain.name, "memory.shared_domains.name")?;
        }
        Ok(())
    }

    pub fn backend_is_in_memory(&self) -> bool {
        matches!(self.backend.as_ref(), "in_memory" | "memory.in_memory")
    }

    pub fn semantic_backend_is_qdrant(&self) -> bool {
        matches!(self.semantic_backend.as_ref(), "qdrant" | "memory.qdrant")
    }

    pub fn semantic_backend_is_sqlite_vec(&self) -> bool {
        matches!(
            self.semantic_backend.as_ref(),
            "sqlite" | "memory.sqlite" | "sqlite_vec" | "memory.sqlite_vec"
        )
    }

    pub fn hydration_settings(&self) -> Result<MemoryHydrationSettings, String> {
        Ok(MemoryHydrationSettings {
            enabled: self.hydration_enabled,
            max_fragments: self.hydrate_max_fragments,
            max_estimated_tokens: self.hydrate_max_estimated_tokens,
            stores: self
                .hydrate_stores
                .iter()
                .map(|store| parse_memory_store(store))
                .collect::<Result<Vec<_>, _>>()?,
            strategy: parse_retrieval_strategy(&self.hydrate_strategy)?,
            allowed_shared_domains: self
                .shared_domains
                .iter()
                .filter(|domain| domain.read)
                .map(|domain| Arc::clone(&domain.name))
                .collect(),
        })
    }
}

impl TemplateConfig {
    fn to_template(&self, config: &WorkspaceConfig) -> Result<OrchestratorTemplate, String> {
        Ok(OrchestratorTemplate {
            name: Arc::clone(&self.name),
            stages: self
                .stages
                .iter()
                .map(|stage| {
                    Ok(Stage {
                        name: Arc::clone(&stage.name),
                        agent: SubAgentSpec {
                            agent_id: AgentId::new(Arc::clone(&stage.agent_id)),
                            policy_id: Arc::clone(&stage.policy_id),
                            metadata: config
                                .subagent_metadata(&stage.agent_id, &stage.policy_id)?,
                        },
                        depends_on: stage.depends_on.clone(),
                    })
                })
                .collect::<Result<Vec<_>, String>>()?,
        })
    }
}

fn parse_domain(input: &str) -> TaskDomain {
    match input {
        "software_dev" | "software-dev" | "software" => TaskDomain::SoftwareDev,
        "content_ops" | "content-ops" | "content" => TaskDomain::ContentOps,
        "research" => TaskDomain::Research,
        "editing" | "edit" => TaskDomain::Editing,
        "general" => TaskDomain::General,
        other => TaskDomain::Custom(Arc::from(other)),
    }
}

fn parse_memory_store(input: &str) -> Result<MemoryStore, String> {
    match normalize_config_token(input).as_str() {
        "working" => Ok(MemoryStore::Working),
        "episodic" => Ok(MemoryStore::Episodic),
        "semantic" => Ok(MemoryStore::Semantic),
        "procedural" => Ok(MemoryStore::Procedural),
        "audit" => Ok(MemoryStore::Audit),
        other => Err(format!(
            "unknown memory store '{other}'; expected working, episodic, semantic, procedural, or audit"
        )),
    }
}

fn parse_retrieval_strategy(input: &str) -> Result<RetrievalStrategy, String> {
    match normalize_config_token(input).as_str() {
        "lexical" => Ok(RetrievalStrategy::Lexical),
        "recency" => Ok(RetrievalStrategy::Recency),
        "hybrid" => Ok(RetrievalStrategy::Hybrid),
        other => Err(format!(
            "unknown memory hydrate_strategy '{other}'; expected lexical, recency, or hybrid"
        )),
    }
}

fn validate_qdrant_config(config: &MemoryQdrantConfig) -> Result<(), String> {
    if config.url.trim().is_empty() {
        return Err("memory.qdrant.url must not be empty".to_owned());
    }
    if !config.url.starts_with("http://") {
        return Err("memory.qdrant.url must use http://".to_owned());
    }
    if config.collection.trim().is_empty() {
        return Err("memory.qdrant.collection must not be empty".to_owned());
    }
    if config.vector_dimensions == 0 {
        return Err("memory.qdrant.vector_dimensions must be greater than 0".to_owned());
    }
    if config.timeout_ms == 0 {
        return Err("memory.qdrant.timeout_ms must be greater than 0".to_owned());
    }
    Ok(())
}

fn validate_sqlite_vec_config(config: &MemorySqliteVecConfig) -> Result<(), String> {
    if config.table.is_empty()
        || !config
            .table
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        || config
            .table
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_digit())
    {
        return Err(
            "memory.sqlite_vec.table must be a non-empty identifier containing only letters, digits, or '_' and must not start with a digit"
                .to_owned(),
        );
    }
    if config.vector_dimensions == 0 {
        return Err("memory.sqlite_vec.vector_dimensions must be greater than 0".to_owned());
    }
    Ok(())
}

fn validate_optional_budget(value: Option<usize>, name: &str) -> Result<(), String> {
    if value == Some(0) {
        Err(format!("{name} must be greater than 0"))
    } else {
        Ok(())
    }
}

fn validate_memory_policy(input: &str, name: &str) -> Result<(), String> {
    match normalize_config_token(input).as_str() {
        "allow" | "deny" | "ask_user" => Ok(()),
        other => Err(format!(
            "{name} has unknown value '{other}'; expected allow, deny, or ask_user"
        )),
    }
}

fn normalize_config_token(input: &str) -> String {
    input.trim().to_ascii_lowercase().replace('-', "_")
}

fn normalize_domain(input: &str, name: &str) -> Result<Arc<str>, String> {
    let normalized = input
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let normalized = normalized.trim_matches('_');
    if normalized.is_empty() {
        Err(format!("{name} must not be empty"))
    } else {
        Ok(Arc::from(normalized))
    }
}

fn normalize_memory_view(input: &str) -> Result<String, String> {
    match normalize_config_token(input).as_str() {
        "none" | "" => Ok("none".to_owned()),
        "shared_readonly" | "shared_read_only" => Ok("shared_readonly".to_owned()),
        "shared_readwrite" | "shared_read_write" => Ok("shared_readwrite".to_owned()),
        other => Err(format!(
            "unknown subagent memory_view '{other}'; expected none, shared_readonly, or shared_readwrite"
        )),
    }
}

fn normalize_memory_tool(input: &str) -> Result<String, String> {
    match normalize_config_token(input).as_str() {
        "read" | "write" | "forget" => Ok(normalize_config_token(input)),
        other => Err(format!(
            "unknown subagent memory_tools entry '{other}'; expected read, write, or forget"
        )),
    }
}

fn subagent_memory_metadata(
    subagent: &SubAgentConfig,
) -> Result<BTreeMap<Arc<str>, serde_json::Value>, String> {
    let memory_view = normalize_memory_view(&subagent.memory_view)?;
    let mut metadata = BTreeMap::new();
    if memory_view == "none" {
        if !subagent.memory_domains.is_empty() {
            return Err(format!(
                "subagent '{}' sets memory_domains without enabling memory_view",
                subagent.id
            ));
        }
        return Ok(metadata);
    }

    let memory_domains = subagent
        .memory_domains
        .iter()
        .map(|domain| normalize_domain(domain, "subagents.memory_domains"))
        .collect::<Result<Vec<_>, _>>()?;
    let memory_tools = subagent
        .memory_tools
        .iter()
        .map(|tool| normalize_memory_tool(tool))
        .collect::<Result<Vec<_>, _>>()?;

    metadata.insert(
        Arc::from("memory_view"),
        serde_json::Value::String(memory_view),
    );
    metadata.insert(
        Arc::from("memory_default_owner"),
        serde_json::Value::String("agent".to_owned()),
    );
    if !memory_domains.is_empty() {
        metadata.insert(
            Arc::from("memory_domains"),
            serde_json::Value::Array(
                memory_domains
                    .iter()
                    .map(|domain| serde_json::Value::String(domain.to_string()))
                    .collect(),
            ),
        );
    }
    if !memory_tools.is_empty() {
        metadata.insert(
            Arc::from("memory_tools"),
            serde_json::Value::Array(
                memory_tools
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
    }
    Ok(metadata)
}

fn parse_dispatch(
    rule: &RoutingRuleConfig,
    templates: &BTreeMap<Arc<str>, OrchestratorTemplate>,
    config: &WorkspaceConfig,
) -> Result<DispatchTarget, String> {
    match rule.dispatch.as_ref() {
        "escalate" => {
            let template_name = rule.template.as_ref().ok_or_else(|| {
                format!(
                    "routing domain '{}' uses escalate without template",
                    rule.domain
                )
            })?;
            let template = templates.get(template_name).cloned().ok_or_else(|| {
                format!(
                    "routing domain '{}' references unknown template '{}'",
                    rule.domain, template_name
                )
            })?;
            Ok(DispatchTarget::Escalate(template))
        }
        "delegate" => {
            let agent_id = rule.agent_id.as_ref().ok_or_else(|| {
                format!(
                    "routing domain '{}' uses delegate without agent_id",
                    rule.domain
                )
            })?;
            let policy_id = rule.policy_id.as_ref().ok_or_else(|| {
                format!(
                    "routing domain '{}' uses delegate without policy_id",
                    rule.domain
                )
            })?;
            if !config
                .subagents
                .iter()
                .any(|subagent| subagent.id == *agent_id && subagent.policy_id == *policy_id)
            {
                return Err(format!(
                    "routing domain '{}' delegates to unknown subagent '{}' with policy '{}'",
                    rule.domain, agent_id, policy_id
                ));
            }
            Ok(DispatchTarget::Delegate(SubAgentSpec {
                agent_id: AgentId::new(Arc::clone(agent_id)),
                policy_id: Arc::clone(policy_id),
                metadata: config.subagent_metadata(agent_id, policy_id)?,
            }))
        }
        "direct" => Ok(DispatchTarget::Direct),
        other => Err(format!(
            "routing domain '{}' has unknown dispatch '{}'",
            rule.domain, other
        )),
    }
}

