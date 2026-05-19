use agentos_interfaces::orchestrator::{
    DispatchTarget, Plan, RoutingRule, RoutingTable, RunContext, SubOrchSpec, TaskDomain,
};
use agentos_proto::{Message, MessageRole};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

pub(super) const ROUTER_CONFIDENCE_THRESHOLD: f32 = 0.60;

pub(super) fn latest_user_content(ctx: &RunContext<'_>) -> Option<Arc<str>> {
    let item = ctx.state.transcript.items.last()?;
    (item.message.role == MessageRole::User).then_some(Arc::clone(&item.message.content))
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
                "dispatch": dispatch_descriptor(&rule.dispatch),
            })
        })
        .collect::<Vec<_>>();
    vec![
        Message::text(
            MessageRole::System,
            "Classify the user's task using only the provided routing domains and their delegate/escalation target descriptions. Prefer the target whose functional description best matches the requested work; do not use keyword matching. Return only JSON with fields: domain, confidence.",
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

fn dispatch_descriptor(dispatch: &DispatchTarget) -> Value {
    match dispatch {
        DispatchTarget::Direct => json!({
            "kind": "direct",
            "target": "parent",
            "description": "Handle directly in the parent run."
        }),
        DispatchTarget::Delegate(spec) => json!({
            "kind": "delegate",
            "agent_id": spec.agent_id.as_str(),
            "policy_id": spec.policy_id,
            "subagent": subagent_descriptor(&spec.metadata),
        }),
        DispatchTarget::Escalate(template) => json!({
            "kind": "escalate",
            "template": template.name,
            "stages": template.stages.iter().map(|stage| {
                json!({
                    "stage": stage.name,
                    "agent_id": stage.agent.agent_id.as_str(),
                    "policy_id": stage.agent.policy_id,
                    "subagent": subagent_descriptor(&stage.agent.metadata),
                    "depends_on": stage.depends_on,
                })
            }).collect::<Vec<_>>(),
        }),
    }
}

fn subagent_descriptor(metadata: &BTreeMap<Arc<str>, Value>) -> Value {
    json!({
        "name": metadata.get("subagent_name").and_then(Value::as_str),
        "description": metadata.get("subagent_description").and_then(Value::as_str),
        "tools": metadata.get("subagent_tools").and_then(Value::as_array).cloned().unwrap_or_default(),
        "skills": metadata.get("subagent_skills").and_then(Value::as_array).cloned().unwrap_or_default(),
        "max_turns": metadata.get("subagent_max_turns").and_then(Value::as_u64),
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_interfaces::orchestrator::{OrchestratorTemplate, Stage, SubAgentSpec, TaskDomain};
    use agentos_proto::AgentId;

    #[test]
    fn routing_prompt_includes_subagent_functional_descriptions() {
        let mut research_metadata = BTreeMap::new();
        research_metadata.insert(
            Arc::from("subagent_name"),
            Value::String("research-subagent".to_owned()),
        );
        research_metadata.insert(
            Arc::from("subagent_description"),
            Value::String("Performs bounded research using configured HTTP resources.".to_owned()),
        );
        research_metadata.insert(
            Arc::from("subagent_tools"),
            Value::Array(vec![Value::String("http".to_owned())]),
        );
        research_metadata.insert(
            Arc::from("subagent_skills"),
            Value::Array(vec![Value::String("web-research".to_owned())]),
        );
        research_metadata.insert(Arc::from("subagent_max_turns"), Value::from(4));
        let table = RoutingTable {
            rules: vec![RoutingRule {
                domain: TaskDomain::Research,
                description: Arc::from("External evidence gathering."),
                examples: Vec::new(),
                dispatch: DispatchTarget::Escalate(OrchestratorTemplate {
                    name: Arc::from("research"),
                    stages: vec![Stage {
                        name: Arc::from("research"),
                        agent: SubAgentSpec {
                            agent_id: AgentId::new("research-subagent"),
                            policy_id: Arc::from("readonly-web"),
                            metadata: research_metadata,
                        },
                        depends_on: Vec::new(),
                    }],
                }),
            }],
            fallback: RoutingRule {
                domain: TaskDomain::General,
                description: Arc::from("General fallback."),
                examples: Vec::new(),
                dispatch: DispatchTarget::Direct,
            },
        };

        let messages = routing_classifier_messages("compare these sources", &table);
        let prompt = messages
            .iter()
            .map(|message| message.content.as_ref())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(prompt.contains("Performs bounded research using configured HTTP resources."));
        assert!(prompt.contains("web-research"));
        assert!(prompt.contains("readonly-web"));
        assert!(prompt.contains("do not use keyword matching"));
    }
}
