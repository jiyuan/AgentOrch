use crate::hooks::Hooks;
use agentos_interfaces::RunState;
use agentos_proto::{SpanId, SpanKind, TraceEvent, TraceSpan};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

pub(crate) fn record_span(
    state: &mut RunState,
    parent_id: Option<SpanId>,
    kind: SpanKind,
    name: impl Into<Arc<str>>,
    fields: BTreeMap<Arc<str>, Value>,
) -> SpanId {
    let id = SpanId::new(format!("span-{}", state.trace_spans.len() + 1));
    state.trace_spans.push(TraceSpan {
        id: id.clone(),
        parent_id,
        kind,
        name: name.into(),
        fields,
    });
    id
}

pub(crate) fn record_event(
    state: &mut RunState,
    hooks: Option<&Hooks>,
    span_id: SpanId,
    name: impl Into<Arc<str>>,
    fields: BTreeMap<Arc<str>, Value>,
) {
    let event = TraceEvent {
        span_id,
        name: name.into(),
        fields,
    };

    if let Some(hooks) = hooks {
        hooks.try_emit(event.clone());
    }

    state.trace_events.push(event);
}

pub(crate) fn run_span_id(state: &RunState) -> Option<SpanId> {
    state
        .trace_spans
        .iter()
        .find(|span| span.kind == SpanKind::Run)
        .map(|span| span.id.clone())
}
