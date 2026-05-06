use crate::approve::{Policy, PolicyError};
use crate::memory::{InMemorySession, MemoryManager};
use crate::r#loop::{InputGuardrailEntry, OutputGuardrailEntry, ToolGuardrailEntry};
use crate::runner::{run_envelope, RunOutcome, RunnerDeps, TraceSink};
use crate::task_workspace::TaskWorkspace;
use crate::tools::ToolRegistry;
use agentos_interfaces::guardrail::{InputGuardrail, OutputGuardrail, ToolGuardrail};
use agentos_interfaces::orchestrator::{Orchestrator, SubAgentSpec};
use agentos_proto::{AgentId, ChannelId, ConversationId, Envelope, Message, MessageRole, RunId};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::task::LocalSet;

#[derive(Debug, Error)]
pub enum SubAgentError {
    #[error("unknown sub-agent '{agent_id:?}' with policy '{policy_id}'")]
    Unknown {
        agent_id: AgentId,
        policy_id: Arc<str>,
    },
    #[error("child policy is not a narrowing of parent policy: {0}")]
    Policy(#[from] PolicyError),
    #[error("sub-agent channel closed")]
    ChannelClosed,
    #[error("sub-agent task failed: {0}")]
    Task(Arc<str>),
    #[error("sub-agent run failed: {0}")]
    Run(Arc<str>),
    #[error("sub-agent paused unexpectedly")]
    Paused,
}

pub struct SubAgentDefinition {
    pub agent_id: AgentId,
    pub policy_id: Arc<str>,
    pub orchestrator: Arc<dyn Orchestrator>,
    pub policy: Policy,
    pub tools: Option<Arc<ToolRegistry>>,
    pub memory_manager: Option<Arc<MemoryManager>>,
    pub max_turns: usize,
    pub input_guardrails: Vec<OwnedInputGuardrailEntry>,
    pub output_guardrails: Vec<OwnedOutputGuardrailEntry>,
    pub tool_guardrails: Vec<OwnedToolGuardrailEntry>,
}

impl SubAgentDefinition {
    pub fn new(
        agent_id: AgentId,
        policy_id: impl Into<Arc<str>>,
        orchestrator: Arc<dyn Orchestrator>,
        policy: Policy,
    ) -> Self {
        Self {
            agent_id,
            policy_id: policy_id.into(),
            orchestrator,
            policy,
            tools: None,
            memory_manager: None,
            max_turns: 4,
            input_guardrails: Vec::new(),
            output_guardrails: Vec::new(),
            tool_guardrails: Vec::new(),
        }
    }

    pub fn with_tools(mut self, tools: Arc<ToolRegistry>) -> Self {
        self.tools = Some(tools);
        self
    }

    pub fn with_memory_manager(mut self, memory_manager: Arc<MemoryManager>) -> Self {
        self.memory_manager = Some(memory_manager);
        self
    }

    pub fn with_max_turns(mut self, max_turns: usize) -> Self {
        self.max_turns = max_turns;
        self
    }

    pub fn with_input_guardrail<T>(mut self, name: impl Into<Arc<str>>, guardrail: T) -> Self
    where
        T: InputGuardrail + 'static,
    {
        self.input_guardrails.push(OwnedInputGuardrailEntry {
            name: name.into(),
            guardrail: Arc::new(guardrail),
        });
        self
    }

    pub fn with_output_guardrail<T>(mut self, name: impl Into<Arc<str>>, guardrail: T) -> Self
    where
        T: OutputGuardrail + 'static,
    {
        self.output_guardrails.push(OwnedOutputGuardrailEntry {
            name: name.into(),
            guardrail: Arc::new(guardrail),
        });
        self
    }

    pub fn with_tool_guardrail<T>(mut self, name: impl Into<Arc<str>>, guardrail: T) -> Self
    where
        T: ToolGuardrail + 'static,
    {
        self.tool_guardrails.push(OwnedToolGuardrailEntry {
            name: name.into(),
            guardrail: Arc::new(guardrail),
        });
        self
    }
}

pub struct OwnedInputGuardrailEntry {
    pub name: Arc<str>,
    pub guardrail: Arc<dyn InputGuardrail>,
}

pub struct OwnedOutputGuardrailEntry {
    pub name: Arc<str>,
    pub guardrail: Arc<dyn OutputGuardrail>,
}

pub struct OwnedToolGuardrailEntry {
    pub name: Arc<str>,
    pub guardrail: Arc<dyn ToolGuardrail>,
}

#[derive(Debug)]
pub struct SubAgentRunOutput {
    pub agent_id: AgentId,
    pub policy_id: Arc<str>,
    pub state: agentos_interfaces::RunState,
    pub message: Message,
}

pub struct SubAgentInvocation {
    definition: Arc<SubAgentDefinition>,
    policy: Policy,
    input: Envelope,
    run_id: RunId,
    channel_capacity: usize,
    trace_sink: Option<Arc<dyn TraceSink>>,
    task_workspace: Option<Arc<TaskWorkspace>>,
}

pub struct SubAgentRegistry {
    definitions: BTreeMap<(AgentId, Arc<str>), Arc<SubAgentDefinition>>,
    channel_capacity: usize,
    trace_sink: Option<Arc<dyn TraceSink>>,
    task_workspace: Option<Arc<TaskWorkspace>>,
}

impl Default for SubAgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SubAgentRegistry {
    pub fn new() -> Self {
        Self {
            definitions: BTreeMap::new(),
            channel_capacity: 1,
            trace_sink: None,
            task_workspace: None,
        }
    }

    pub fn with_channel_capacity(mut self, channel_capacity: usize) -> Self {
        self.channel_capacity = channel_capacity.max(1);
        self
    }

    pub fn with_trace_sink(mut self, trace_sink: Arc<dyn TraceSink>) -> Self {
        self.trace_sink = Some(trace_sink);
        self
    }

    pub fn with_task_workspace(mut self, task_workspace: Arc<TaskWorkspace>) -> Self {
        self.task_workspace = Some(task_workspace);
        self
    }

    pub fn register(&mut self, definition: SubAgentDefinition) {
        self.definitions.insert(
            (
                definition.agent_id.clone(),
                Arc::clone(&definition.policy_id),
            ),
            Arc::new(definition),
        );
    }

    pub fn prepare(
        &self,
        spec: &SubAgentSpec,
        parent_policy: &Policy,
        input: Envelope,
        run_id: RunId,
    ) -> Result<SubAgentInvocation, SubAgentError> {
        let definition = self
            .definitions
            .get(&(spec.agent_id.clone(), Arc::clone(&spec.policy_id)))
            .cloned()
            .ok_or_else(|| SubAgentError::Unknown {
                agent_id: spec.agent_id.clone(),
                policy_id: Arc::clone(&spec.policy_id),
            })?;
        let child_policy = Policy::narrow(parent_policy, &definition.policy)?;
        Ok(SubAgentInvocation {
            definition,
            policy: child_policy,
            input,
            run_id,
            channel_capacity: self.channel_capacity,
            trace_sink: self.trace_sink.clone(),
            task_workspace: self.task_workspace.clone(),
        })
    }
}

impl SubAgentInvocation {
    pub async fn run(self) -> Result<SubAgentRunOutput, SubAgentError> {
        let (input_tx, mut input_rx) = mpsc::channel(self.channel_capacity);
        let (output_tx, mut output_rx) = mpsc::channel(self.channel_capacity);

        input_tx
            .send(self.input)
            .await
            .map_err(|_| SubAgentError::ChannelClosed)?;

        let definition = self.definition;
        let child_policy = self.policy;
        let run_id = self.run_id;
        let trace_sink = self.trace_sink;
        let task_workspace = self.task_workspace;
        let local = LocalSet::new();
        let handle = local.spawn_local(async move {
            let Some(input) = input_rx.recv().await else {
                return Err(SubAgentError::ChannelClosed);
            };
            let session = InMemorySession::default();
            let input_guardrails = definition
                .input_guardrails
                .iter()
                .map(|entry| InputGuardrailEntry {
                    name: Arc::clone(&entry.name),
                    guardrail: entry.guardrail.as_ref(),
                })
                .collect::<Vec<_>>();
            let output_guardrails = definition
                .output_guardrails
                .iter()
                .map(|entry| OutputGuardrailEntry {
                    name: Arc::clone(&entry.name),
                    guardrail: entry.guardrail.as_ref(),
                })
                .collect::<Vec<_>>();
            let tool_guardrails = definition
                .tool_guardrails
                .iter()
                .map(|entry| ToolGuardrailEntry {
                    name: Arc::clone(&entry.name),
                    guardrail: entry.guardrail.as_ref(),
                })
                .collect::<Vec<_>>();
            let deps = RunnerDeps {
                orchestrator: definition.orchestrator.as_ref(),
                session: &session,
                memory_manager: definition.memory_manager.as_deref(),
                hooks: None,
                max_turns: definition.max_turns,
                active_agent: definition.agent_id.clone(),
                tools: definition.tools.as_deref(),
                trace_sink: trace_sink.as_deref(),
                task_workspace: task_workspace.as_deref(),
                policy: &child_policy,
                subagents: None,
                input_guardrails: &input_guardrails,
                output_guardrails: &output_guardrails,
                tool_guardrails: &tool_guardrails,
            };
            let result = match run_envelope(input, run_id, &deps).await {
                Ok(RunOutcome::Finished { state, output }) => Ok(SubAgentRunOutput {
                    agent_id: definition.agent_id.clone(),
                    policy_id: Arc::clone(&definition.policy_id),
                    state,
                    message: output.message,
                }),
                Ok(RunOutcome::Paused(_)) => Err(SubAgentError::Paused),
                Err(err) => Err(SubAgentError::Run(Arc::from(err.to_string()))),
            };
            output_tx
                .send(result)
                .await
                .map_err(|_| SubAgentError::ChannelClosed)
        });

        local
            .run_until(async move {
                let output = output_rx.recv().await.ok_or(SubAgentError::ChannelClosed)?;
                handle
                    .await
                    .map_err(|err| SubAgentError::Task(Arc::from(err.to_string())))??;
                output
            })
            .await
    }
}

pub fn child_input_envelope(
    spec: &SubAgentSpec,
    parent_state: &agentos_interfaces::RunState,
) -> Envelope {
    let message = spec
        .metadata
        .get("prompt")
        .and_then(Value::as_str)
        .map(|prompt| Message::text(MessageRole::User, prompt))
        .or_else(|| {
            parent_state
                .transcript
                .items
                .last()
                .map(|item| item.message.clone())
        })
        .unwrap_or_else(|| Message::text(MessageRole::User, ""));
    let mut metadata = spec.metadata.clone();
    metadata.insert(
        Arc::from("kind"),
        Value::String("subagent_input".to_owned()),
    );
    metadata.insert(
        Arc::from("parent_run_id"),
        Value::String(parent_state.run_id.as_str().to_owned()),
    );

    Envelope {
        channel_id: ChannelId::new(format!("subagent:{}", spec.agent_id.as_str())),
        conversation_id: ConversationId::new(format!(
            "{}:{}",
            parent_state.run_id.as_str(),
            spec.agent_id.as_str()
        )),
        sender: Arc::from(parent_state.active_agent.as_str()),
        message,
        metadata,
    }
}

pub fn child_run_id(spec: &SubAgentSpec, parent_state: &agentos_interfaces::RunState) -> RunId {
    RunId::new(format!(
        "{}:{}",
        parent_state.run_id.as_str(),
        spec.agent_id.as_str()
    ))
}
