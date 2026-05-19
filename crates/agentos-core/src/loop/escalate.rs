use super::delegate::{execute_delegate, DelegateOutcome};
use super::telemetry::{
    record_suborch_failure, record_telemetry_event, suborch_stage_agent_telemetry_fields,
    suborch_stage_telemetry_fields, suborch_telemetry_fields,
};
use super::{LoopDeps, RunError};
use crate::subagents::{SubAgentError, SubAgentPausedRun, SubAgentRunOutput};
use crate::trace;
use agentos_interfaces::orchestrator::SubOrchSpec;
use agentos_interfaces::run_state::RunState;
use agentos_proto::SpanKind;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

pub(super) enum EscalateOutcome {
    Finished(Vec<(Arc<str>, SubAgentRunOutput)>),
    Paused {
        stage_agent: agentos_interfaces::orchestrator::SubAgentSpec,
        paused: Box<SubAgentPausedRun>,
    },
}

pub(super) async fn execute_escalate(
    state: &mut RunState,
    deps: &LoopDeps<'_>,
    spec: &SubOrchSpec,
) -> Result<EscalateOutcome, RunError> {
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
            Ok(DelegateOutcome::Finished(result)) => result,
            Ok(DelegateOutcome::Paused(paused)) => {
                let mut fields =
                    suborch_stage_agent_telemetry_fields(spec, &stage_name, &stage_agent);
                fields.insert(Arc::from("status"), Value::String("paused".to_owned()));
                record_telemetry_event(
                    state,
                    deps.hooks,
                    span_id.clone(),
                    "suborch_stage_call_paused",
                    fields,
                );
                return Ok(EscalateOutcome::Paused {
                    stage_agent,
                    paused: Box::new(paused),
                });
            }
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
    Ok(EscalateOutcome::Finished(ordered))
}
