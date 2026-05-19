use super::LoopDeps;
use crate::hooks::Hooks;
use crate::subagents::SubAgentError;
use crate::trace;
use agentos_interfaces::orchestrator::{Plan, Stage, SubAgentSpec, SubOrchSpec};
use agentos_interfaces::run_state::RunState;
use agentos_proto::SpanId;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use tracing::info;

pub(super) fn plan_assignment_fields(state: &RunState, plan: &Plan) -> BTreeMap<Arc<str>, Value> {
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
        Plan::ResumeSubAgent { spec, .. } => {
            fields.insert(
                Arc::from("plan_kind"),
                Value::String("resume_subagent".to_owned()),
            );
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
    }
    fields
}

pub(super) fn record_telemetry_event(
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

pub(super) fn subagent_telemetry_fields(spec: &SubAgentSpec) -> BTreeMap<Arc<str>, Value> {
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

pub(super) fn suborch_telemetry_fields(spec: &SubOrchSpec) -> BTreeMap<Arc<str>, Value> {
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

pub(super) fn suborch_stage_telemetry_fields(
    spec: &SubOrchSpec,
    stage: &Stage,
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

pub(super) fn suborch_stage_agent_telemetry_fields(
    spec: &SubOrchSpec,
    stage_name: &Arc<str>,
    stage_agent: &SubAgentSpec,
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

pub(super) fn record_subagent_failure(
    state: &mut RunState,
    deps: &LoopDeps<'_>,
    span_id: SpanId,
    spec: &SubAgentSpec,
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

pub(super) fn record_suborch_failure(
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
