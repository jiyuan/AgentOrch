use agentos_interfaces::orchestrator::{
    DispatchTarget, Plan, RoutingRule, RoutingTable, RunContext, SubOrchSpec, TaskDomain,
};
use agentos_proto::{Message, MessageRole};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

pub(super) const ROUTER_CONFIDENCE_THRESHOLD: f32 = 0.60;

pub(super) fn classify_task(input: &str) -> Option<TaskDomain> {
    let input = input.to_ascii_lowercase();
    if input.contains("implement")
        || input.contains("debug")
        || input.contains("bug")
        || input.contains("code")
        || input.contains("software")
    {
        Some(TaskDomain::SoftwareDev)
    } else if input.contains("content") || input.contains("publish") || input.contains("campaign") {
        Some(TaskDomain::ContentOps)
    } else if input.contains("research")
        || input.contains("investigate")
        || input.contains("summarize")
    {
        Some(TaskDomain::Research)
    } else if input.contains("edit") || input.contains("rewrite") || input.contains("proofread") {
        Some(TaskDomain::Editing)
    } else {
        None
    }
}

pub(super) fn latest_user_content(ctx: &RunContext<'_>) -> Option<Arc<str>> {
    let item = ctx.state.transcript.items.last()?;
    (item.message.role == MessageRole::User).then_some(Arc::clone(&item.message.content))
}

pub(super) fn rule_for_domain<'a>(
    routing_table: &'a RoutingTable,
    domain: &TaskDomain,
) -> Option<&'a RoutingRule> {
    routing_table
        .rules
        .iter()
        .find(|rule| &rule.domain == domain)
        .or_else(|| (&routing_table.fallback.domain == domain).then_some(&routing_table.fallback))
}

pub(super) fn rule_for_domain_key<'a>(
    routing_table: &'a RoutingTable,
    domain: &str,
) -> Option<&'a RoutingRule> {
    routing_table
        .rules
        .iter()
        .find(|rule| domain_key(&rule.domain) == domain)
        .or_else(|| {
            (domain_key(&routing_table.fallback.domain) == domain)
                .then_some(&routing_table.fallback)
        })
}

pub(super) fn routing_classifier_messages(
    input: &str,
    routing_table: &RoutingTable,
) -> Vec<Message> {
    let domains = routing_table
        .rules
        .iter()
        .chain(std::iter::once(&routing_table.fallback))
        .map(|rule| {
            json!({
                "domain": domain_key(&rule.domain),
                "description": rule.description,
                "examples": rule.examples,
            })
        })
        .collect::<Vec<_>>();
    vec![
        Message::text(
            MessageRole::System,
            "Classify the user's task using only the provided routing domains. Return only JSON with fields: domain, confidence.",
        ),
        Message::text(
            MessageRole::User,
            format!(
                "Routing domains:\n{}\n\nUser prompt:\n{}",
                serde_json::to_string_pretty(&domains).unwrap_or_else(|_| "[]".to_owned()),
                input
            ),
        ),
    ]
}

#[derive(Debug, Deserialize)]
pub(super) struct RoutingDecision {
    pub(super) domain: String,
    #[serde(default = "default_routing_confidence")]
    pub(super) confidence: f32,
}

fn default_routing_confidence() -> f32 {
    0.0
}

pub(super) fn parse_routing_decision(input: &str) -> Option<RoutingDecision> {
    let json = extract_json_object(input.trim())?;
    let mut decision = serde_json::from_str::<RoutingDecision>(json).ok()?;
    decision.domain = normalize_domain_key(&decision.domain);
    Some(decision)
}

fn extract_json_object(input: &str) -> Option<&str> {
    let trimmed = input
        .strip_prefix("```json")
        .or_else(|| input.strip_prefix("```"))
        .unwrap_or(input)
        .trim();
    let trimmed = trimmed.strip_suffix("```").unwrap_or(trimmed).trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Some(trimmed);
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    (start <= end).then_some(&trimmed[start..=end])
}

fn domain_key(domain: &TaskDomain) -> String {
    match domain {
        TaskDomain::SoftwareDev => "software_dev".to_owned(),
        TaskDomain::ContentOps => "content_ops".to_owned(),
        TaskDomain::Research => "research".to_owned(),
        TaskDomain::Editing => "editing".to_owned(),
        TaskDomain::General => "general".to_owned(),
        TaskDomain::Custom(value) => value.to_string(),
    }
}

fn normalize_domain_key(input: &str) -> String {
    input.trim().to_ascii_lowercase().replace('-', "_")
}

pub(super) fn materialize_dispatch(
    ctx: &RunContext<'_>,
    input: &str,
    dispatch: &DispatchTarget,
) -> Option<Plan> {
    match dispatch {
        DispatchTarget::Direct => None,
        DispatchTarget::Delegate(spec) => {
            let mut spec = spec.clone();
            spec.metadata
                .entry(Arc::from("prompt"))
                .or_insert_with(|| Value::String(input.to_owned()));
            Some(Plan::Delegate(spec))
        }
        DispatchTarget::Escalate(template) => {
            let mut metadata = BTreeMap::new();
            metadata.insert(Arc::from("prompt"), Value::String(input.to_owned()));
            Some(Plan::Escalate(SubOrchSpec {
                template: template.clone(),
                task_id: ctx.system.task_id.clone(),
                policy_id: Arc::from("default"),
                metadata,
            }))
        }
    }
}
