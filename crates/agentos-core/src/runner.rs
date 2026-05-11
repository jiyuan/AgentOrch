use crate::approve::Policy;
use crate::hooks::Hooks;
mod episodes;

use crate::memory::MemoryManager;
use crate::r#loop::{
    resume_approved, FinalOutput, InputGuardrailEntry, LoopDeps, OutputGuardrailEntry, RunError,
    RunLoopState, StartCtx, ToolGuardrailEntry,
};
use crate::subagents::SubAgentRegistry;
use crate::task_workspace::{TaskWorkspace, TaskWorkspaceError};
use crate::tools::ToolRegistry;
use crate::trace;
use agentos_interfaces::orchestrator::Orchestrator;
use agentos_interfaces::run_state::InterruptionAction;
use agentos_interfaces::session::{Item, Session, SessionError};
use agentos_interfaces::RunState;
use agentos_proto::{
    AgentId, ChannelId, ConversationId, Envelope, InterruptionId, Message, MessageRole, RunId,
    SpanKind, TaskId,
};
use episodes::{record_denied_episode, record_error_episode, record_finished_episode, EpisodeSeed};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;
use tracing::info;

#[derive(Debug, Error)]
pub enum RunnerError {
    #[error("run loop failed: {0}")]
    Run(#[from] RunError),
    #[error("session failed: {0}")]
    Session(#[from] SessionError),
    #[error("paused run state I/O failed for {path}: {source}")]
    StateIo {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("paused run state JSON failed for {path}: {source}")]
    StateJson {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("trace record I/O failed for {path}: {source}")]
    TraceIo {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("trace record JSON failed for {path}: {source}")]
    TraceJson {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("task workspace failed: {0}")]
    TaskWorkspace(#[from] TaskWorkspaceError),
}

pub struct RunnerDeps<'a> {
    pub orchestrator: &'a dyn Orchestrator,
    pub session: &'a dyn Session,
    pub memory_manager: Option<&'a MemoryManager>,
    pub hooks: Option<&'a Hooks>,
    pub max_turns: usize,
    pub active_agent: AgentId,
    pub tools: Option<&'a ToolRegistry>,
    pub trace_sink: Option<&'a dyn TraceSink>,
    pub task_workspace: Option<&'a TaskWorkspace>,
    pub policy: &'a Policy,
    pub subagents: Option<&'a SubAgentRegistry>,
    pub input_guardrails: &'a [InputGuardrailEntry<'a>],
    pub output_guardrails: &'a [OutputGuardrailEntry<'a>],
    pub tool_guardrails: &'a [ToolGuardrailEntry<'a>],
}

pub trait TraceSink: Send + Sync {
    fn persist(
        &self,
        state: &RunState,
        span_start: usize,
        event_start: usize,
        phase: &'static str,
    ) -> Result<(), RunnerError>;
}

#[derive(Clone, Debug)]
pub struct JsonlTraceSink {
    dir: PathBuf,
}

impl JsonlTraceSink {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
}

impl TraceSink for JsonlTraceSink {
    fn persist(
        &self,
        state: &RunState,
        span_start: usize,
        event_start: usize,
        phase: &'static str,
    ) -> Result<(), RunnerError> {
        persist_trace_records(state, &self.dir, span_start, event_start, phase)
    }
}

#[derive(Debug)]
pub enum RunOutcome {
    Finished { state: RunState, output: Envelope },
    Paused(RunState),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PausedRun {
    pub channel_id: ChannelId,
    pub conversation_id: ConversationId,
    pub state: RunState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResumeDecision {
    Approve,
    Reject { reason: Arc<str> },
}

pub fn approval_prompt_envelope(paused: &PausedRun, sender: Arc<str>) -> Option<Envelope> {
    let approval = paused.state.pending_approvals.first()?;
    let mut metadata = BTreeMap::new();
    metadata.insert(
        Arc::from("kind"),
        Value::String("approval_prompt".to_owned()),
    );
    metadata.insert(
        Arc::from("approval_id"),
        Value::String(approval.id.as_str().to_owned()),
    );
    metadata.insert(
        Arc::from("run_id"),
        Value::String(paused.state.run_id.as_str().to_owned()),
    );
    let (action_kind, action_label) = approval_action_label(&approval.action);
    metadata.insert(
        Arc::from("action_kind"),
        Value::String(action_kind.to_owned()),
    );
    metadata.insert(
        Arc::from("action_label"),
        Value::String(action_label.clone()),
    );
    if let InterruptionAction::ToolCall(call) = &approval.action {
        metadata.insert(
            Arc::from("tool_name"),
            Value::String(call.name.as_ref().to_owned()),
        );
    }

    Some(Envelope {
        channel_id: paused.channel_id.clone(),
        conversation_id: paused.conversation_id.clone(),
        sender,
        message: Message {
            role: MessageRole::Assistant,
            content: Arc::from(format!(
                "Approve {} for {} '{}'? Reply y to approve, anything else to reject.",
                approval.id.as_str(),
                action_kind,
                action_label
            )),
            attachments: Vec::new(),
            metadata: BTreeMap::new(),
        },
        metadata,
    })
}

fn approval_action_label(action: &InterruptionAction) -> (&'static str, String) {
    match action {
        InterruptionAction::ToolCall(call) => ("tool", call.name.as_ref().to_owned()),
        InterruptionAction::Delegate(spec) => (
            "delegate",
            format!("{} ({})", spec.agent_id.as_str(), spec.policy_id),
        ),
        InterruptionAction::Escalate(spec) => (
            "escalate",
            format!("{} ({})", spec.template.name, spec.task_id.as_str()),
        ),
        InterruptionAction::Handoff { agent_id, .. } => ("handoff", agent_id.as_str().to_owned()),
    }
}

pub async fn run_envelope(
    input: Envelope,
    run_id: RunId,
    deps: &RunnerDeps<'_>,
) -> Result<RunOutcome, RunnerError> {
    let mut transcript = deps.session.load(&input.conversation_id).await?;
    let persisted_len = transcript.items.len();
    let mut input_metadata = input.metadata.clone();
    input_metadata
        .entry(Arc::from("conversation_id"))
        .or_insert_with(|| Value::String(input.conversation_id.as_str().to_owned()));
    input_metadata
        .entry(Arc::from("channel_id"))
        .or_insert_with(|| Value::String(input.channel_id.as_str().to_owned()));
    input_metadata
        .entry(Arc::from("sender"))
        .or_insert_with(|| Value::String(input.sender.as_ref().to_owned()));
    let input_item = Item {
        message: input.message.clone(),
        metadata: input_metadata,
    };
    transcript.items.push(input_item);

    let mut state = RunState::new(run_id.clone(), deps.active_agent.clone());
    state.transcript = transcript;
    let task_session = activate_task_workspace_for_run(&mut state, &input, deps)?;
    let episode_seed = EpisodeSeed::from_input(
        &input,
        &run_id,
        &deps.active_agent,
        state
            .task_id
            .clone()
            .unwrap_or_else(|| task_id_for_state(&state)),
    );
    record_run_start(&mut state, deps.hooks);

    let loop_deps = LoopDeps {
        orchestrator: deps.orchestrator,
        max_turns: deps.max_turns,
        hooks: deps.hooks,
        tools: deps.tools,
        task_workspace: deps.task_workspace,
        policy: deps.policy,
        subagents: deps.subagents,
        input_guardrails: deps.input_guardrails,
        output_guardrails: deps.output_guardrails,
        tool_guardrails: deps.tool_guardrails,
    };
    let mut current = RunLoopState::Start(StartCtx { state });

    loop {
        current = match current.step(&loop_deps).await {
            Ok(next) => next,
            Err(err) => {
                record_error_episode(&episode_seed, &err, deps).await;
                return Err(err.into());
            }
        };
        match current {
            RunLoopState::Finish(final_output) => {
                let (state, output) = finish(
                    input.channel_id,
                    input.conversation_id,
                    persisted_len,
                    0,
                    0,
                    final_output,
                    deps,
                )
                .await?;
                return Ok(RunOutcome::Finished { state, output });
            }
            RunLoopState::Paused(state) => {
                let append_items = state.transcript.items[persisted_len..].to_vec();
                deps.session
                    .append(&input.conversation_id, append_items)
                    .await?;
                persist_task_session_items(
                    task_session.as_ref(),
                    "paused",
                    &state.transcript.items[persisted_len..],
                )?;
                persist_trace_records_with_sink(&state, deps.trace_sink, 0, 0, "paused")?;
                return Ok(RunOutcome::Paused(state));
            }
            next => current = next,
        }
    }
}

pub async fn resume_run(
    mut paused: PausedRun,
    approval_id: &InterruptionId,
    decision: ResumeDecision,
    deps: &RunnerDeps<'_>,
) -> Result<RunOutcome, RunnerError> {
    let persisted_len = paused.state.transcript.items.len();
    let trace_span_start = paused.state.trace_spans.len();
    let trace_event_start = paused.state.trace_events.len();
    let task_session = activate_task_workspace_for_resume(&mut paused.state, deps)?;
    let rejected_reason = match decision {
        ResumeDecision::Approve => {
            paused.state.approve(approval_id);
            None
        }
        ResumeDecision::Reject { reason } => {
            paused.state.reject(approval_id, Arc::clone(&reason));
            Some(reason)
        }
    };
    if let Some(reason) = rejected_reason {
        record_denied_episode(&paused.state, &paused.conversation_id, &reason, deps).await;
        return Err(RunError::ApprovalDenied { reason }.into());
    }
    let episode_seed = EpisodeSeed::from_state(&paused.state, &paused.conversation_id);

    let loop_deps = LoopDeps {
        orchestrator: deps.orchestrator,
        max_turns: deps.max_turns,
        hooks: deps.hooks,
        tools: deps.tools,
        task_workspace: deps.task_workspace,
        policy: deps.policy,
        subagents: deps.subagents,
        input_guardrails: deps.input_guardrails,
        output_guardrails: deps.output_guardrails,
        tool_guardrails: deps.tool_guardrails,
    };
    let mut current = match resume_approved(paused.state) {
        Ok(current) => current,
        Err(err) => {
            record_error_episode(&episode_seed, &err, deps).await;
            return Err(err.into());
        }
    };

    loop {
        current = match current.step(&loop_deps).await {
            Ok(next) => next,
            Err(err) => {
                record_error_episode(&episode_seed, &err, deps).await;
                return Err(err.into());
            }
        };
        match current {
            RunLoopState::Finish(final_output) => {
                let (state, output) = finish(
                    paused.channel_id,
                    paused.conversation_id,
                    persisted_len,
                    trace_span_start,
                    trace_event_start,
                    final_output,
                    deps,
                )
                .await?;
                return Ok(RunOutcome::Finished { state, output });
            }
            RunLoopState::Paused(state) => {
                persist_task_session_items(task_session.as_ref(), "paused", &[])?;
                persist_trace_records_with_sink(
                    &state,
                    deps.trace_sink,
                    trace_span_start,
                    trace_event_start,
                    "paused",
                )?;
                return Ok(RunOutcome::Paused(state));
            }
            next => current = next,
        }
    }
}

pub fn save_paused_run(path: &Path, paused: &PausedRun) -> Result<(), RunnerError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| RunnerError::StateIo {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let encoded = serde_json::to_vec_pretty(paused).map_err(|source| RunnerError::StateJson {
        path: path.to_path_buf(),
        source,
    })?;
    std::fs::write(path, encoded).map_err(|source| RunnerError::StateIo {
        path: path.to_path_buf(),
        source,
    })
}

pub fn load_paused_run(path: &Path) -> Result<PausedRun, RunnerError> {
    let encoded = std::fs::read(path).map_err(|source| RunnerError::StateIo {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&encoded).map_err(|source| RunnerError::StateJson {
        path: path.to_path_buf(),
        source,
    })
}

pub fn delete_paused_run(path: &Path) -> Result<(), RunnerError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(RunnerError::StateIo {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn persist_trace_records(
    state: &RunState,
    trace_dir: &Path,
    span_start: usize,
    event_start: usize,
    phase: &'static str,
) -> Result<(), RunnerError> {
    std::fs::create_dir_all(trace_dir).map_err(|source| RunnerError::TraceIo {
        path: trace_dir.to_path_buf(),
        source,
    })?;
    let path = trace_dir.join(format!("{}.jsonl", trace_file_stem(&state.run_id)));
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|source| RunnerError::TraceIo {
            path: path.clone(),
            source,
        })?;

    for (index, span) in state.trace_spans.iter().enumerate().skip(span_start) {
        let record = json!({
            "record_type": "span",
            "phase": phase,
            "run_id": state.run_id.as_str(),
            "active_agent": state.active_agent.as_str(),
            "index": index,
            "span": span,
        });
        write_trace_record(&mut file, &path, &record)?;
    }
    for (index, event) in state.trace_events.iter().enumerate().skip(event_start) {
        let record = json!({
            "record_type": "event",
            "phase": phase,
            "run_id": state.run_id.as_str(),
            "active_agent": state.active_agent.as_str(),
            "index": index,
            "event": event,
        });
        write_trace_record(&mut file, &path, &record)?;
    }
    Ok(())
}

fn persist_trace_records_with_sink(
    state: &RunState,
    trace_sink: Option<&dyn TraceSink>,
    span_start: usize,
    event_start: usize,
    phase: &'static str,
) -> Result<(), RunnerError> {
    let Some(trace_sink) = trace_sink else {
        return Ok(());
    };
    trace_sink.persist(state, span_start, event_start, phase)
}

fn write_trace_record(
    file: &mut std::fs::File,
    path: &Path,
    record: &Value,
) -> Result<(), RunnerError> {
    let encoded = serde_json::to_string(record).map_err(|source| RunnerError::TraceJson {
        path: path.to_path_buf(),
        source,
    })?;
    writeln!(file, "{encoded}").map_err(|source| RunnerError::TraceIo {
        path: path.to_path_buf(),
        source,
    })
}

fn trace_file_stem(run_id: &RunId) -> String {
    run_id
        .as_str()
        .chars()
        .map(|ch| match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | '-' | ':' => ch,
            _ => '_',
        })
        .collect()
}

async fn finish(
    channel_id: ChannelId,
    conversation_id: ConversationId,
    persisted_len: usize,
    trace_span_start: usize,
    trace_event_start: usize,
    final_output: FinalOutput,
    deps: &RunnerDeps<'_>,
) -> Result<(RunState, Envelope), RunnerError> {
    let mut state = final_output.state;
    let output_item = Item {
        message: final_output.message.clone(),
        metadata: BTreeMap::new(),
    };
    state.transcript.items.push(output_item);
    record_run_finish(&mut state, deps.hooks);

    let append_items = state.transcript.items[persisted_len..].to_vec();
    deps.session.append(&conversation_id, append_items).await?;
    persist_task_session_items(
        active_task_session(&state, deps).as_ref(),
        "finished",
        &state.transcript.items[persisted_len..],
    )?;
    persist_trace_records_with_sink(
        &state,
        deps.trace_sink,
        trace_span_start,
        trace_event_start,
        "finished",
    )?;
    let mut output_metadata = BTreeMap::new();
    if let Some(metadata) = record_finished_episode(&state, &conversation_id, deps).await {
        output_metadata.extend(metadata);
    }

    let output = Envelope {
        channel_id,
        conversation_id,
        sender: Arc::from(deps.active_agent.as_str()),
        message: final_output.message,
        metadata: output_metadata,
    };

    Ok((state, output))
}

#[derive(Clone, Debug)]
struct ActiveTaskSession<'a> {
    workspace: &'a TaskWorkspace,
    task_id: TaskId,
    session_id: Arc<str>,
}

fn activate_task_workspace_for_run<'a>(
    state: &mut RunState,
    input: &Envelope,
    deps: &'a RunnerDeps<'_>,
) -> Result<Option<ActiveTaskSession<'a>>, RunnerError> {
    let Some(workspace) = deps.task_workspace else {
        return Ok(None);
    };
    let task_id = task_id_for_run(state, input);
    let session_id = Arc::from(sanitize_file_stem(state.run_id.as_str()));
    workspace.init_task(&task_id)?;
    state.task_id = Some(task_id.clone());
    state.task_session_id = Some(Arc::clone(&session_id));
    let active = ActiveTaskSession {
        workspace,
        task_id,
        session_id,
    };
    append_task_session_event(
        &active,
        json!({
            "event": "session_started",
            "phase": "run",
            "run_id": state.run_id.as_str(),
            "active_agent": state.active_agent.as_str(),
            "conversation_id": input.conversation_id.as_str(),
            "channel_id": input.channel_id.as_str(),
        }),
    )?;
    append_task_session_event(
        &active,
        json!({
            "event": "input",
            "message": input.message,
            "metadata": input.metadata,
        }),
    )?;
    Ok(Some(active))
}

fn activate_task_workspace_for_resume<'a>(
    state: &mut RunState,
    deps: &'a RunnerDeps<'_>,
) -> Result<Option<ActiveTaskSession<'a>>, RunnerError> {
    let Some(workspace) = deps.task_workspace else {
        return Ok(None);
    };
    let task_id = state
        .task_id
        .clone()
        .unwrap_or_else(|| task_id_for_state(state));
    let session_id = Arc::from(format!(
        "{}-resume-{}",
        sanitize_file_stem(state.run_id.as_str()),
        state.trace_events.len()
    ));
    workspace.init_task(&task_id)?;
    state.task_id = Some(task_id.clone());
    state.task_session_id = Some(Arc::clone(&session_id));
    let active = ActiveTaskSession {
        workspace,
        task_id,
        session_id,
    };
    append_task_session_event(
        &active,
        json!({
            "event": "session_started",
            "phase": "resume",
            "run_id": state.run_id.as_str(),
            "active_agent": state.active_agent.as_str(),
        }),
    )?;
    Ok(Some(active))
}

fn active_task_session<'a>(
    state: &RunState,
    deps: &'a RunnerDeps<'_>,
) -> Option<ActiveTaskSession<'a>> {
    let workspace = deps.task_workspace?;
    Some(ActiveTaskSession {
        workspace,
        task_id: state
            .task_id
            .clone()
            .unwrap_or_else(|| task_id_for_state(state)),
        session_id: state
            .task_session_id
            .clone()
            .unwrap_or_else(|| Arc::from(sanitize_file_stem(state.run_id.as_str()))),
    })
}

fn task_id_for_run(state: &RunState, input: &Envelope) -> TaskId {
    input
        .metadata
        .get("task_id")
        .and_then(Value::as_str)
        .map(TaskId::new)
        .unwrap_or_else(|| task_id_for_state(state))
}

fn task_id_for_state(state: &RunState) -> TaskId {
    if state.active_agent.as_str() == "general-subagent" {
        TaskId::new("general")
    } else {
        TaskId::new(state.run_id.as_str())
    }
}

fn persist_task_session_items(
    active: Option<&ActiveTaskSession<'_>>,
    phase: &'static str,
    items: &[Item],
) -> Result<(), RunnerError> {
    let Some(active) = active else {
        return Ok(());
    };
    for (index, item) in items.iter().enumerate() {
        append_task_session_event(
            active,
            json!({
                "event": "transcript_item",
                "phase": phase,
                "index": index,
                "message": item.message,
                "metadata": item.metadata,
            }),
        )?;
    }
    append_task_session_event(
        active,
        json!({
            "event": "session_finished",
            "phase": phase,
        }),
    )
}

fn append_task_session_event(
    active: &ActiveTaskSession<'_>,
    event: Value,
) -> Result<(), RunnerError> {
    active
        .workspace
        .append_session_event(&active.task_id, &active.session_id, &event)?;
    Ok(())
}

fn sanitize_file_stem(input: &str) -> String {
    input
        .chars()
        .map(|ch| match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | '-' | ':' => ch,
            _ => '_',
        })
        .collect()
}

fn record_run_start(state: &mut RunState, hooks: Option<&Hooks>) {
    let mut fields = BTreeMap::new();
    fields.insert(
        Arc::from("run_id"),
        Value::String(state.run_id.as_str().to_owned()),
    );
    fields.insert(
        Arc::from("active_agent"),
        Value::String(state.active_agent.as_str().to_owned()),
    );
    let span_id = trace::record_span(state, None, SpanKind::Run, "run", fields);
    trace::record_event(
        state,
        hooks,
        span_id.clone(),
        "run_started",
        BTreeMap::new(),
    );
    info!(
        run_id = state.run_id.as_str(),
        active_agent = state.active_agent.as_str(),
        "run_started"
    );
}

fn record_run_finish(state: &mut RunState, hooks: Option<&Hooks>) {
    let span_id = trace::run_span_id(state)
        .unwrap_or_else(|| trace::record_span(state, None, SpanKind::Run, "run", BTreeMap::new()));
    trace::record_event(state, hooks, span_id, "run_finished", BTreeMap::new());
    info!(
        run_id = state.run_id.as_str(),
        active_agent = state.active_agent.as_str(),
        "run_finished"
    );
}
