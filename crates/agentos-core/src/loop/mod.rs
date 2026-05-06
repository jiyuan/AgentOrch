use crate::approve::{tool_call_approval_id, Policy, PolicyDecision};
use crate::hooks::Hooks;
use crate::subagents::{
    child_input_envelope, child_run_id, SubAgentError, SubAgentRegistry, SubAgentRunOutput,
};
use crate::task_workspace::{TaskWorkspace, TaskWorkspaceError};
use crate::tools::{ToolRegistry, ToolRegistryError};
use crate::trace;
use agentos_interfaces::guardrail::{
    GuardrailError, GuardrailOutcome, Input, InputGuardrail, OutputGuardrail, ToolGuardrail,
};
use agentos_interfaces::orchestrator::{Orchestrator, Plan, RunContext, SubOrchSpec};
use agentos_interfaces::run_state::{ApprovalStatus, Interruption, InterruptionAction, RunState};
use agentos_interfaces::session::Item;
use agentos_proto::{
    AgentId, InterruptionId, Message, MessageRole, SpanId, SpanKind, ToolCall, ToolResult,
    ToolStatus,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use thiserror::Error;
use tracing::info;

#[derive(Debug, Error)]
pub enum RunError {
    #[error("run has already finished")]
    AlreadyDone,
    #[error("paused run must be resumed through the approval path")]
    NotResumable,
    #[error("maximum turn count exceeded")]
    MaxTurnsExceeded,
    #[error("orchestrator failed: {0}")]
    Orchestrator(#[from] agentos_interfaces::orchestrator::OrchestratorError),
    #[error("guardrail backend failed: {0}")]
    Guardrail(#[from] GuardrailError),
    #[error("guardrail '{guardrail}' tripped: {reason}")]
    GuardrailTripped {
        guardrail: Arc<str>,
        reason: Arc<str>,
    },
    #[error("tool execution failed: {0}")]
    Tool(#[from] ToolRegistryError),
    #[error("sub-agent execution failed: {0}")]
    SubAgent(#[from] SubAgentError),
    #[error("task workspace failed: {0}")]
    TaskWorkspace(#[from] TaskWorkspaceError),
    #[error("approval denied: {reason}")]
    ApprovalDenied { reason: Arc<str> },
    #[error("approval cannot pause this action yet: {reason}")]
    ApprovalUnsupported { reason: Arc<str> },
}

pub struct LoopDeps<'a> {
    pub orchestrator: &'a dyn Orchestrator,
    pub max_turns: usize,
    pub hooks: Option<&'a Hooks>,
    pub tools: Option<&'a ToolRegistry>,
    pub task_workspace: Option<&'a TaskWorkspace>,
    pub policy: &'a Policy,
    pub subagents: Option<&'a SubAgentRegistry>,
    pub input_guardrails: &'a [InputGuardrailEntry<'a>],
    pub output_guardrails: &'a [OutputGuardrailEntry<'a>],
    pub tool_guardrails: &'a [ToolGuardrailEntry<'a>],
}

pub struct InputGuardrailEntry<'a> {
    pub name: Arc<str>,
    pub guardrail: &'a dyn InputGuardrail,
}

pub struct OutputGuardrailEntry<'a> {
    pub name: Arc<str>,
    pub guardrail: &'a dyn OutputGuardrail,
}

pub struct ToolGuardrailEntry<'a> {
    pub name: Arc<str>,
    pub guardrail: &'a dyn ToolGuardrail,
}

#[derive(Debug)]
pub enum RunLoopState {
    Start(StartCtx),
    Plan(PlanCtx),
    Approve(ApproveCtx),
    Act(ActCtx),
    Observe(ObserveCtx),
    Paused(RunState),
    Finish(FinalOutput),
}

impl RunLoopState {
    pub async fn step(self, deps: &LoopDeps<'_>) -> Result<Self, RunError> {
        match self {
            Self::Start(ctx) => start(ctx, deps).await,
            Self::Plan(ctx) => plan(ctx, deps).await,
            Self::Approve(ctx) => approve(ctx, deps).await,
            Self::Act(ctx) => act(ctx, deps).await,
            Self::Observe(ctx) => observe(ctx).await,
            Self::Paused(_) => Err(RunError::NotResumable),
            Self::Finish(_) => Err(RunError::AlreadyDone),
        }
    }
}

pub fn resume_approved(state: RunState) -> Result<RunLoopState, RunError> {
    let mut state = state;
    if let Some(reason) = state.take_rejected_reason() {
        return Err(RunError::ApprovalDenied { reason });
    }

    let turns = resume_turns(&state);
    let Some(action) = state.take_approved_action() else {
        return Err(RunError::NotResumable);
    };
    let plan = match action {
        InterruptionAction::ToolCall(call) => Plan::CallTool(call),
        InterruptionAction::Delegate(spec) => Plan::Delegate(spec),
        InterruptionAction::Escalate(spec) => Plan::Escalate(spec),
        InterruptionAction::Handoff { agent_id, payload } => Plan::Handoff(agent_id, payload),
    };
    Ok(RunLoopState::Act(ActCtx { state, plan, turns }))
}

#[derive(Debug)]
pub struct StartCtx {
    pub state: RunState,
}

#[derive(Debug)]
pub struct PlanCtx {
    pub state: RunState,
    pub turns: usize,
}

#[derive(Debug)]
pub struct ApproveCtx {
    pub state: RunState,
    pub plan: Plan,
    pub turns: usize,
}

#[derive(Debug)]
pub struct ActCtx {
    pub state: RunState,
    pub plan: Plan,
    pub turns: usize,
}

#[derive(Debug)]
pub struct ObserveCtx {
    pub state: RunState,
    pub turns: usize,
}

#[derive(Debug)]
pub struct FinalOutput {
    pub state: RunState,
    pub message: Message,
}

async fn start(ctx: StartCtx, deps: &LoopDeps<'_>) -> Result<RunLoopState, RunError> {
    info!(
        run_id = ctx.state.run_id.as_str(),
        active_agent = ctx.state.active_agent.as_str(),
        "run_loop_start"
    );
    if let Some(item) = ctx.state.transcript.items.last() {
        let run_ctx = RunContext::from_state(&ctx.state);
        let input = Input {
            message: item.message.clone(),
        };
        for entry in deps.input_guardrails {
            let outcome = entry.guardrail.check(&input, &run_ctx).await?;
            ensure_guardrail_passed(&entry.name, outcome)?;
        }
    }
    Ok(RunLoopState::Plan(PlanCtx {
        state: ctx.state,
        turns: 0,
    }))
}

async fn plan(ctx: PlanCtx, deps: &LoopDeps<'_>) -> Result<RunLoopState, RunError> {
    if ctx.turns >= deps.max_turns {
        return Err(RunError::MaxTurnsExceeded);
    }

    let mut state = ctx.state;
    let mut fields = BTreeMap::new();
    fields.insert(Arc::from("turn"), Value::from(ctx.turns));
    let parent_id = trace::run_span_id(&state);
    let plan_span_id = trace::record_span(&mut state, parent_id, SpanKind::State, "plan", fields);
    trace::record_event(
        &mut state,
        deps.hooks,
        plan_span_id.clone(),
        "plan_started",
        BTreeMap::new(),
    );

    let hydrate_span_id = trace::record_span(
        &mut state,
        Some(plan_span_id.clone()),
        SpanKind::State,
        "orchestrator.hydrate",
        BTreeMap::new(),
    );
    trace::record_event(
        &mut state,
        deps.hooks,
        hydrate_span_id.clone(),
        "hydrate_started",
        BTreeMap::new(),
    );
    let mut run_ctx = RunContext::from_state(&state);
    deps.orchestrator.hydrate(&mut run_ctx).await?;
    let mut hydrate_fields = BTreeMap::new();
    hydrate_fields.insert(
        Arc::from("memory_fragments"),
        Value::from(run_ctx.memory_fragments.len()),
    );
    hydrate_fields.insert(
        Arc::from("resources"),
        Value::from(run_ctx.resource_index.entries.len()),
    );
    for key in [
        "memory_hydration_candidate_count",
        "memory_hydration_selected_count",
        "memory_hydration_namespace_count",
    ] {
        if let Some(value) = run_ctx.system.metadata.get(key) {
            hydrate_fields.insert(Arc::from(key), value.clone());
        }
    }
    let plan = deps.orchestrator.plan(&run_ctx).await?;
    drop(run_ctx);
    trace::record_event(
        &mut state,
        deps.hooks,
        hydrate_span_id,
        "hydrate_finished",
        hydrate_fields,
    );
    trace::record_span(
        &mut state,
        Some(plan_span_id.clone()),
        SpanKind::Llm,
        "orchestrator.plan",
        BTreeMap::new(),
    );
    let assignment_fields = plan_assignment_fields(&state, &plan);
    record_telemetry_event(
        &mut state,
        deps.hooks,
        plan_span_id.clone(),
        "orchestrator_task_assigned",
        assignment_fields,
    );
    trace::record_event(
        &mut state,
        deps.hooks,
        plan_span_id,
        "plan_finished",
        BTreeMap::new(),
    );
    info!(
        run_id = state.run_id.as_str(),
        active_agent = state.active_agent.as_str(),
        turn = ctx.turns,
        "plan_finished"
    );

    match plan {
        Plan::Reply(message) => {
            let run_ctx = RunContext::from_state(&state);
            for entry in deps.output_guardrails {
                let outcome = entry.guardrail.check(&message, &run_ctx).await?;
                ensure_guardrail_passed(&entry.name, outcome)?;
            }
            Ok(RunLoopState::Finish(FinalOutput { state, message }))
        }
        plan => Ok(RunLoopState::Approve(ApproveCtx {
            state,
            plan,
            turns: ctx.turns,
        })),
    }
}

async fn approve(ctx: ApproveCtx, deps: &LoopDeps<'_>) -> Result<RunLoopState, RunError> {
    match approve_transition(ctx, deps.policy) {
        ApproveTransition::Allow { state, plan, turns } => {
            Ok(RunLoopState::Act(ActCtx { state, plan, turns }))
        }
        ApproveTransition::Deny { reason } => Err(RunError::ApprovalDenied { reason }),
        ApproveTransition::Pause { state } => Ok(RunLoopState::Paused(state)),
        ApproveTransition::Unsupported { reason } => Err(RunError::ApprovalUnsupported { reason }),
    }
}

enum ApproveTransition {
    Allow {
        state: RunState,
        plan: Plan,
        turns: usize,
    },
    Deny {
        reason: Arc<str>,
    },
    Pause {
        state: RunState,
    },
    Unsupported {
        reason: Arc<str>,
    },
}

fn approve_transition(ctx: ApproveCtx, policy: &Policy) -> ApproveTransition {
    match policy.decide(&ctx.plan) {
        PolicyDecision::Allow => ApproveTransition::Allow {
            state: ctx.state,
            plan: ctx.plan,
            turns: ctx.turns,
        },
        PolicyDecision::Deny { reason } => ApproveTransition::Deny { reason },
        PolicyDecision::AskUser { reason } => pause_for_approval(ctx, reason),
    }
}

fn pause_for_approval(ctx: ApproveCtx, reason: Arc<str>) -> ApproveTransition {
    let (approval_id, action) = match ctx.plan {
        Plan::CallTool(call) => (
            tool_call_approval_id(&call),
            InterruptionAction::ToolCall(call),
        ),
        Plan::Delegate(spec) => (
            delegate_approval_id(&spec),
            InterruptionAction::Delegate(spec),
        ),
        Plan::Escalate(spec) => (
            escalate_approval_id(&spec),
            InterruptionAction::Escalate(spec),
        ),
        Plan::Handoff(agent_id, payload) => (
            handoff_approval_id(&agent_id),
            InterruptionAction::Handoff { agent_id, payload },
        ),
        Plan::Reply(_) => return ApproveTransition::Unsupported { reason },
    };

    let mut state = ctx.state;
    state.pending_approvals.push(Interruption {
        id: InterruptionId::new(approval_id),
        action,
        status: ApprovalStatus::Pending,
    });
    ApproveTransition::Pause { state }
}

fn delegate_approval_id(spec: &agentos_interfaces::orchestrator::SubAgentSpec) -> Arc<str> {
    Arc::from(format!(
        "approval-delegate-{}-{}",
        spec.agent_id.as_str(),
        spec.policy_id
    ))
}

fn handoff_approval_id(agent_id: &agentos_proto::AgentId) -> Arc<str> {
    Arc::from(format!("approval-handoff-{}", agent_id.as_str()))
}

fn escalate_approval_id(spec: &SubOrchSpec) -> Arc<str> {
    Arc::from(format!(
        "approval-escalate-{}-{}",
        spec.template.name,
        spec.task_id.as_str()
    ))
}

fn plan_assignment_fields(state: &RunState, plan: &Plan) -> BTreeMap<Arc<str>, Value> {
    let mut fields = BTreeMap::new();
    fields.insert(
        Arc::from("active_agent"),
        Value::String(state.active_agent.as_str().to_owned()),
    );
    match plan {
        Plan::Reply(message) => {
            fields.insert(Arc::from("plan_kind"), Value::String("reply".to_owned()));
            fields.insert(
                Arc::from("target_type"),
                Value::String("assistant".to_owned()),
            );
            fields.insert(
                Arc::from("message_role"),
                Value::String(format!("{:?}", message.role)),
            );
            fields.insert(
                Arc::from("content_bytes"),
                Value::from(message.content.len()),
            );
        }
        Plan::CallTool(call) => {
            fields.insert(Arc::from("plan_kind"), Value::String("tool".to_owned()));
            fields.insert(Arc::from("target_type"), Value::String("tool".to_owned()));
            fields.insert(
                Arc::from("tool_call_id"),
                Value::String(call.id.as_str().to_owned()),
            );
            fields.insert(
                Arc::from("tool_name"),
                Value::String(call.name.as_ref().to_owned()),
            );
        }
        Plan::Handoff(agent_id, payload) => {
            fields.insert(Arc::from("plan_kind"), Value::String("handoff".to_owned()));
            fields.insert(Arc::from("target_type"), Value::String("agent".to_owned()));
            fields.insert(
                Arc::from("target_agent_id"),
                Value::String(agent_id.as_str().to_owned()),
            );
            fields.insert(Arc::from("has_payload"), Value::Bool(payload.is_some()));
        }
        Plan::Delegate(spec) => {
            fields.insert(Arc::from("plan_kind"), Value::String("delegate".to_owned()));
            fields.insert(
                Arc::from("target_type"),
                Value::String("subagent".to_owned()),
            );
            fields.insert(
                Arc::from("subagent_id"),
                Value::String(spec.agent_id.as_str().to_owned()),
            );
            fields.insert(
                Arc::from("policy_id"),
                Value::String(spec.policy_id.as_ref().to_owned()),
            );
        }
        Plan::Escalate(spec) => {
            fields.insert(Arc::from("plan_kind"), Value::String("escalate".to_owned()));
            fields.insert(
                Arc::from("target_type"),
                Value::String("suborch".to_owned()),
            );
            fields.insert(
                Arc::from("template"),
                Value::String(spec.template.name.as_ref().to_owned()),
            );
            fields.insert(
                Arc::from("task_id"),
                Value::String(spec.task_id.as_str().to_owned()),
            );
            fields.insert(
                Arc::from("policy_id"),
                Value::String(spec.policy_id.as_ref().to_owned()),
            );
            fields.insert(
                Arc::from("stage_count"),
                Value::from(spec.template.stages.len()),
            );
        }
    }
    fields
}

fn record_telemetry_event(
    state: &mut RunState,
    hooks: Option<&Hooks>,
    span_id: SpanId,
    name: &'static str,
    fields: BTreeMap<Arc<str>, Value>,
) {
    let fields_json = serde_json::to_string(&fields).unwrap_or_else(|_| "{}".to_owned());
    trace::record_event(state, hooks, span_id, name, fields);
    info!(
        run_id = state.run_id.as_str(),
        active_agent = state.active_agent.as_str(),
        telemetry_event = name,
        telemetry_fields = %fields_json,
        "orchestration_telemetry"
    );
}

async fn act(ctx: ActCtx, deps: &LoopDeps<'_>) -> Result<RunLoopState, RunError> {
    let mut state = ctx.state;
    match ctx.plan {
        Plan::CallTool(call) => {
            let result = execute_tool(&mut state, deps, call).await?;
            state.transcript.items.push(tool_result_item(&result));
        }
        Plan::Delegate(spec) => {
            let result = execute_delegate(&mut state, deps, &spec).await?;
            state.transcript.items.push(subagent_result_item(&result));
        }
        Plan::Escalate(spec) => {
            let result = execute_escalate(&mut state, deps, &spec).await?;
            state
                .transcript
                .items
                .push(suborchestrator_result_item(&spec, result));
        }
        Plan::Handoff(agent_id, payload) => {
            execute_handoff(&mut state, deps, agent_id, payload);
        }
        Plan::Reply(_) => {}
    }

    Ok(RunLoopState::Observe(ObserveCtx {
        state,
        turns: ctx.turns + 1,
    }))
}

async fn execute_delegate(
    state: &mut RunState,
    deps: &LoopDeps<'_>,
    spec: &agentos_interfaces::orchestrator::SubAgentSpec,
) -> Result<SubAgentRunOutput, RunError> {
    let parent_id = trace::run_span_id(state);
    let mut fields = BTreeMap::new();
    fields.insert(
        Arc::from("subagent_id"),
        Value::String(spec.agent_id.as_str().to_owned()),
    );
    fields.insert(
        Arc::from("policy_id"),
        Value::String(spec.policy_id.as_ref().to_owned()),
    );
    let span_id = trace::record_span(
        state,
        parent_id,
        SpanKind::Handoff,
        format!("delegate.{}", spec.agent_id.as_str()),
        fields,
    );
    trace::record_event(
        state,
        deps.hooks,
        span_id.clone(),
        "subagent_started",
        BTreeMap::new(),
    );

    let mut create_fields = subagent_telemetry_fields(spec);
    let input = child_input_envelope(spec, state);
    let run_id = child_run_id(spec, state);
    create_fields.insert(
        Arc::from("child_run_id"),
        Value::String(run_id.as_str().to_owned()),
    );
    create_fields.insert(
        Arc::from("conversation_id"),
        Value::String(input.conversation_id.as_str().to_owned()),
    );
    create_fields.insert(
        Arc::from("metadata_keys"),
        Value::from(input.metadata.len()),
    );
    record_telemetry_event(
        state,
        deps.hooks,
        span_id.clone(),
        "subagent_create_started",
        create_fields.clone(),
    );

    let subagents = match deps.subagents {
        Some(subagents) => subagents,
        None => {
            let error = SubAgentError::Unknown {
                agent_id: spec.agent_id.clone(),
                policy_id: Arc::clone(&spec.policy_id),
            };
            record_subagent_failure(state, deps, span_id, spec, "subagent_create_failed", &error);
            return Err(error.into());
        }
    };
    let invocation = match subagents.prepare(spec, deps.policy, input, run_id) {
        Ok(invocation) => invocation,
        Err(error) => {
            record_subagent_failure(state, deps, span_id, spec, "subagent_create_failed", &error);
            return Err(error.into());
        }
    };
    record_telemetry_event(
        state,
        deps.hooks,
        span_id.clone(),
        "subagent_created",
        create_fields,
    );
    record_telemetry_event(
        state,
        deps.hooks,
        span_id.clone(),
        "subagent_call_started",
        subagent_telemetry_fields(spec),
    );
    let result = match invocation.run().await {
        Ok(result) => result,
        Err(error) => {
            record_subagent_failure(state, deps, span_id, spec, "subagent_call_failed", &error);
            return Err(error.into());
        }
    };

    let mut fields = BTreeMap::new();
    fields.insert(
        Arc::from("child_run_id"),
        Value::String(result.state.run_id.as_str().to_owned()),
    );
    fields.insert(
        Arc::from("trace_spans"),
        Value::from(result.state.trace_spans.len()),
    );
    fields.insert(
        Arc::from("trace_events"),
        Value::from(result.state.trace_events.len()),
    );
    record_telemetry_event(
        state,
        deps.hooks,
        span_id.clone(),
        "subagent_call_finished",
        fields.clone(),
    );
    trace::record_event(
        state,
        deps.hooks,
        span_id.clone(),
        "subagent_finished",
        fields,
    );
    let mut teardown_fields = subagent_telemetry_fields(spec);
    teardown_fields.insert(Arc::from("status"), Value::String("succeeded".to_owned()));
    teardown_fields.insert(
        Arc::from("child_run_id"),
        Value::String(result.state.run_id.as_str().to_owned()),
    );
    record_telemetry_event(
        state,
        deps.hooks,
        span_id,
        "subagent_teardown",
        teardown_fields,
    );
    Ok(result)
}

fn subagent_telemetry_fields(
    spec: &agentos_interfaces::orchestrator::SubAgentSpec,
) -> BTreeMap<Arc<str>, Value> {
    let mut fields = BTreeMap::new();
    fields.insert(
        Arc::from("subagent_id"),
        Value::String(spec.agent_id.as_str().to_owned()),
    );
    fields.insert(
        Arc::from("policy_id"),
        Value::String(spec.policy_id.as_ref().to_owned()),
    );
    fields
}

fn record_subagent_failure(
    state: &mut RunState,
    deps: &LoopDeps<'_>,
    span_id: SpanId,
    spec: &agentos_interfaces::orchestrator::SubAgentSpec,
    event_name: &'static str,
    error: &SubAgentError,
) {
    let mut fields = subagent_telemetry_fields(spec);
    fields.insert(Arc::from("status"), Value::String("failed".to_owned()));
    fields.insert(Arc::from("error"), Value::String(error.to_string()));
    record_telemetry_event(
        state,
        deps.hooks,
        span_id.clone(),
        event_name,
        fields.clone(),
    );
    record_telemetry_event(state, deps.hooks, span_id, "subagent_teardown", fields);
}

async fn execute_escalate(
    state: &mut RunState,
    deps: &LoopDeps<'_>,
    spec: &SubOrchSpec,
) -> Result<Vec<(Arc<str>, SubAgentRunOutput)>, RunError> {
    let parent_id = trace::run_span_id(state);
    let mut fields = BTreeMap::new();
    fields.insert(
        Arc::from("template"),
        Value::String(spec.template.name.as_ref().to_owned()),
    );
    fields.insert(
        Arc::from("task_id"),
        Value::String(spec.task_id.as_str().to_owned()),
    );
    let span_id = trace::record_span(
        state,
        parent_id,
        SpanKind::Handoff,
        format!("escalate.{}", spec.template.name),
        fields,
    );
    trace::record_event(
        state,
        deps.hooks,
        span_id.clone(),
        "suborchestrator_started",
        BTreeMap::new(),
    );

    let create_fields = suborch_telemetry_fields(spec);
    record_telemetry_event(
        state,
        deps.hooks,
        span_id.clone(),
        "suborch_create_started",
        create_fields.clone(),
    );

    if let Some(workspace) = deps.task_workspace {
        if let Err(error) = workspace.init_task(&spec.task_id) {
            record_suborch_failure(
                state,
                deps,
                span_id,
                spec,
                "suborch_create_failed",
                &error.to_string(),
            );
            return Err(error.into());
        }
        if let Err(error) = workspace.write_suborchestrator_graph(&spec.task_id, &spec.template) {
            record_suborch_failure(
                state,
                deps,
                span_id,
                spec,
                "suborch_create_failed",
                &error.to_string(),
            );
            return Err(error.into());
        }
    }
    record_telemetry_event(
        state,
        deps.hooks,
        span_id.clone(),
        "suborch_created",
        create_fields,
    );
    record_telemetry_event(
        state,
        deps.hooks,
        span_id.clone(),
        "suborch_call_started",
        suborch_telemetry_fields(spec),
    );

    let mut pending = spec.template.stages.clone();
    let mut completed: BTreeMap<Arc<str>, ()> = BTreeMap::new();
    let mut ordered = Vec::new();
    while !pending.is_empty() {
        let Some(index) = pending.iter().position(|stage| {
            stage
                .depends_on
                .iter()
                .all(|dependency| completed.contains_key(dependency))
        }) else {
            let error = RunError::SubAgent(SubAgentError::Run(Arc::from(format!(
                "sub-orchestrator '{}' has unsatisfied or cyclic dependencies",
                spec.template.name
            ))));
            record_suborch_failure(
                state,
                deps,
                span_id,
                spec,
                "suborch_call_failed",
                &error.to_string(),
            );
            return Err(error);
        };
        let stage = pending.remove(index);
        let stage_name = Arc::clone(&stage.name);
        record_telemetry_event(
            state,
            deps.hooks,
            span_id.clone(),
            "suborch_stage_assigned",
            suborch_stage_telemetry_fields(spec, &stage),
        );
        let mut stage_agent = stage.agent.clone();
        if !stage_agent.metadata.contains_key("prompt") {
            if let Some(prompt) = spec.metadata.get("prompt").cloned().or_else(|| {
                state
                    .transcript
                    .items
                    .last()
                    .map(|item| Value::String(item.message.content.to_string()))
            }) {
                stage_agent.metadata.insert(Arc::from("prompt"), prompt);
            }
        }
        record_telemetry_event(
            state,
            deps.hooks,
            span_id.clone(),
            "suborch_stage_call_started",
            suborch_stage_agent_telemetry_fields(spec, &stage_name, &stage_agent),
        );
        let result = match execute_delegate(state, deps, &stage_agent).await {
            Ok(result) => result,
            Err(error) => {
                let mut fields =
                    suborch_stage_agent_telemetry_fields(spec, &stage_name, &stage_agent);
                fields.insert(Arc::from("status"), Value::String("failed".to_owned()));
                fields.insert(Arc::from("error"), Value::String(error.to_string()));
                record_telemetry_event(
                    state,
                    deps.hooks,
                    span_id.clone(),
                    "suborch_stage_call_failed",
                    fields,
                );
                record_suborch_failure(
                    state,
                    deps,
                    span_id,
                    spec,
                    "suborch_call_failed",
                    &error.to_string(),
                );
                return Err(error);
            }
        };
        let mut fields = suborch_stage_agent_telemetry_fields(spec, &stage_name, &stage_agent);
        fields.insert(Arc::from("status"), Value::String("succeeded".to_owned()));
        fields.insert(
            Arc::from("child_run_id"),
            Value::String(result.state.run_id.as_str().to_owned()),
        );
        record_telemetry_event(
            state,
            deps.hooks,
            span_id.clone(),
            "suborch_stage_call_finished",
            fields,
        );
        ordered.push((Arc::clone(&stage_name), result));
        completed.insert(stage_name, ());
    }

    let mut fields = BTreeMap::new();
    fields.insert(Arc::from("stages"), Value::from(ordered.len()));
    record_telemetry_event(
        state,
        deps.hooks,
        span_id.clone(),
        "suborch_call_finished",
        fields.clone(),
    );
    trace::record_event(
        state,
        deps.hooks,
        span_id.clone(),
        "suborchestrator_finished",
        fields.clone(),
    );
    let mut teardown_fields = suborch_telemetry_fields(spec);
    teardown_fields.insert(Arc::from("status"), Value::String("succeeded".to_owned()));
    teardown_fields.insert(Arc::from("stages"), Value::from(ordered.len()));
    record_telemetry_event(
        state,
        deps.hooks,
        span_id,
        "suborch_teardown",
        teardown_fields,
    );
    Ok(ordered)
}

fn suborch_telemetry_fields(spec: &SubOrchSpec) -> BTreeMap<Arc<str>, Value> {
    let mut fields = BTreeMap::new();
    fields.insert(
        Arc::from("template"),
        Value::String(spec.template.name.as_ref().to_owned()),
    );
    fields.insert(
        Arc::from("task_id"),
        Value::String(spec.task_id.as_str().to_owned()),
    );
    fields.insert(
        Arc::from("policy_id"),
        Value::String(spec.policy_id.as_ref().to_owned()),
    );
    fields.insert(
        Arc::from("stage_count"),
        Value::from(spec.template.stages.len()),
    );
    fields
}

fn suborch_stage_telemetry_fields(
    spec: &SubOrchSpec,
    stage: &agentos_interfaces::orchestrator::Stage,
) -> BTreeMap<Arc<str>, Value> {
    let mut fields = suborch_telemetry_fields(spec);
    fields.insert(
        Arc::from("stage"),
        Value::String(stage.name.as_ref().to_owned()),
    );
    fields.insert(
        Arc::from("subagent_id"),
        Value::String(stage.agent.agent_id.as_str().to_owned()),
    );
    fields.insert(
        Arc::from("stage_policy_id"),
        Value::String(stage.agent.policy_id.as_ref().to_owned()),
    );
    fields.insert(
        Arc::from("depends_on_count"),
        Value::from(stage.depends_on.len()),
    );
    fields
}

fn suborch_stage_agent_telemetry_fields(
    spec: &SubOrchSpec,
    stage_name: &Arc<str>,
    stage_agent: &agentos_interfaces::orchestrator::SubAgentSpec,
) -> BTreeMap<Arc<str>, Value> {
    let mut fields = suborch_telemetry_fields(spec);
    fields.insert(
        Arc::from("stage"),
        Value::String(stage_name.as_ref().to_owned()),
    );
    fields.insert(
        Arc::from("subagent_id"),
        Value::String(stage_agent.agent_id.as_str().to_owned()),
    );
    fields.insert(
        Arc::from("stage_policy_id"),
        Value::String(stage_agent.policy_id.as_ref().to_owned()),
    );
    fields
}

fn record_suborch_failure(
    state: &mut RunState,
    deps: &LoopDeps<'_>,
    span_id: SpanId,
    spec: &SubOrchSpec,
    event_name: &'static str,
    error: &str,
) {
    let mut fields = suborch_telemetry_fields(spec);
    fields.insert(Arc::from("status"), Value::String("failed".to_owned()));
    fields.insert(Arc::from("error"), Value::String(error.to_owned()));
    record_telemetry_event(
        state,
        deps.hooks,
        span_id.clone(),
        event_name,
        fields.clone(),
    );
    record_telemetry_event(state, deps.hooks, span_id, "suborch_teardown", fields);
}

fn execute_handoff(
    state: &mut RunState,
    deps: &LoopDeps<'_>,
    agent_id: AgentId,
    payload: Option<Value>,
) {
    let from_agent = state.active_agent.clone();
    let parent_id = trace::run_span_id(state);
    let mut fields = BTreeMap::new();
    fields.insert(
        Arc::from("from_agent"),
        Value::String(from_agent.as_str().to_owned()),
    );
    fields.insert(
        Arc::from("to_agent"),
        Value::String(agent_id.as_str().to_owned()),
    );
    if let Some(payload) = payload {
        fields.insert(Arc::from("payload"), payload);
    }
    let span_id = trace::record_span(
        state,
        parent_id,
        SpanKind::Handoff,
        format!("handoff.{}", agent_id.as_str()),
        fields,
    );
    trace::record_event(
        state,
        deps.hooks,
        span_id.clone(),
        "handoff_started",
        BTreeMap::new(),
    );

    state.active_agent = agent_id;

    let mut fields = BTreeMap::new();
    fields.insert(
        Arc::from("active_agent"),
        Value::String(state.active_agent.as_str().to_owned()),
    );
    trace::record_event(state, deps.hooks, span_id, "handoff_finished", fields);
}

async fn observe(ctx: ObserveCtx) -> Result<RunLoopState, RunError> {
    Ok(RunLoopState::Plan(PlanCtx {
        state: ctx.state,
        turns: ctx.turns,
    }))
}

async fn execute_tool(
    state: &mut RunState,
    deps: &LoopDeps<'_>,
    call: ToolCall,
) -> Result<ToolResult, RunError> {
    let parent_id = trace::run_span_id(state);
    let mut fields = BTreeMap::new();
    fields.insert(
        Arc::from("tool_name"),
        Value::String(call.name.as_ref().to_owned()),
    );
    fields.insert(
        Arc::from("tool_call_id"),
        Value::String(call.id.as_str().to_owned()),
    );
    let tool_span_id = trace::record_span(
        state,
        parent_id,
        SpanKind::Tool,
        format!("tool.{}", call.name),
        fields,
    );
    trace::record_event(
        state,
        deps.hooks,
        tool_span_id.clone(),
        "tool_started",
        BTreeMap::new(),
    );

    {
        let run_ctx = RunContext::from_state(state);
        for entry in deps.tool_guardrails {
            let outcome = entry.guardrail.check_call(&call, &run_ctx).await?;
            ensure_guardrail_passed(&entry.name, outcome)?;
        }
    }

    let tools = deps
        .tools
        .ok_or_else(|| ToolRegistryError::UnknownTool(Arc::clone(&call.name)))?;
    let result = {
        let run_ctx = RunContext::from_state(state);
        tools.call_with_context(&call, &run_ctx).await?
    };

    {
        let run_ctx = RunContext::from_state(state);
        for entry in deps.tool_guardrails {
            let outcome = entry.guardrail.check_result(&result, &run_ctx).await?;
            ensure_guardrail_passed(&entry.name, outcome)?;
        }
    }

    let mut fields = BTreeMap::new();
    fields.insert(
        Arc::from("status"),
        Value::String(tool_status_name(&result.status).to_owned()),
    );
    trace::record_event(state, deps.hooks, tool_span_id, "tool_finished", fields);
    Ok(result)
}

fn ensure_guardrail_passed(name: &Arc<str>, outcome: GuardrailOutcome) -> Result<(), RunError> {
    match outcome {
        GuardrailOutcome::Passed => Ok(()),
        GuardrailOutcome::Tripped(reason) => Err(RunError::GuardrailTripped {
            guardrail: Arc::clone(name),
            reason,
        }),
    }
}

fn tool_result_item(result: &ToolResult) -> Item {
    let mut metadata = result.metadata.clone();
    metadata.insert(
        Arc::from("tool_call_id"),
        Value::String(result.call_id.as_str().to_owned()),
    );
    metadata.insert(
        Arc::from("tool_status"),
        Value::String(tool_status_name(&result.status).to_owned()),
    );
    Item {
        message: Message {
            role: MessageRole::Tool,
            content: Arc::clone(&result.content),
            metadata,
        },
        metadata: BTreeMap::new(),
    }
}

fn subagent_result_item(result: &SubAgentRunOutput) -> Item {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        Arc::from("kind"),
        Value::String("subagent_result".to_owned()),
    );
    metadata.insert(
        Arc::from("subagent_id"),
        Value::String(result.agent_id.as_str().to_owned()),
    );
    metadata.insert(
        Arc::from("policy_id"),
        Value::String(result.policy_id.as_ref().to_owned()),
    );
    metadata.insert(
        Arc::from("child_run_id"),
        Value::String(result.state.run_id.as_str().to_owned()),
    );
    Item {
        message: Message {
            role: MessageRole::Tool,
            content: Arc::clone(&result.message.content),
            metadata,
        },
        metadata: BTreeMap::new(),
    }
}

fn suborchestrator_result_item(
    spec: &SubOrchSpec,
    results: Vec<(Arc<str>, SubAgentRunOutput)>,
) -> Item {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        Arc::from("kind"),
        Value::String("suborchestrator_result".to_owned()),
    );
    metadata.insert(
        Arc::from("template"),
        Value::String(spec.template.name.as_ref().to_owned()),
    );
    metadata.insert(
        Arc::from("task_id"),
        Value::String(spec.task_id.as_str().to_owned()),
    );
    metadata.insert(Arc::from("stages"), Value::from(results.len()));
    let content = if results.is_empty() {
        format!(
            "sub-orchestrator '{}' completed with no stages",
            spec.template.name
        )
    } else {
        results
            .iter()
            .map(|(stage, result)| format!("{}: {}", stage, result.message.content))
            .collect::<Vec<_>>()
            .join("\n")
    };
    Item {
        message: Message {
            role: MessageRole::Tool,
            content: Arc::from(content),
            metadata,
        },
        metadata: BTreeMap::new(),
    }
}

fn tool_status_name(status: &ToolStatus) -> &'static str {
    match status {
        ToolStatus::Succeeded => "succeeded",
        ToolStatus::Failed => "failed",
        ToolStatus::Denied => "denied",
    }
}

fn resume_turns(state: &RunState) -> usize {
    state
        .trace_spans
        .iter()
        .rev()
        .find(|span| span.kind == SpanKind::State && span.name.as_ref() == "plan")
        .and_then(|span| span.fields.get("turn"))
        .and_then(Value::as_u64)
        .map_or(0, |turn| turn as usize)
}
