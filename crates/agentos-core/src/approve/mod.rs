use agentos_interfaces::orchestrator::Plan;
use agentos_proto::ToolCall;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyDecision {
    Allow,
    Deny { reason: Arc<str> },
    AskUser { reason: Arc<str> },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyVerb {
    Allow,
    Deny,
    AskUser,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum PolicyAction {
    Any,
    Tool(Arc<str>),
    Handoff,
    Delegate,
    Escalate,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PolicyRule {
    pub action: PolicyAction,
    pub decision: PolicyVerb,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<Arc<str>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub arg_equals: BTreeMap<Arc<str>, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Policy {
    pub rules: Vec<PolicyRule>,
    pub default_decision: PolicyVerb,
}

#[derive(Debug, Error)]
pub enum PolicyError {
    #[error("child policy widens parent permissions for {0}")]
    Widened(Arc<str>),
    #[error("invalid policy YAML at line {line}: {message}")]
    InvalidYaml { line: usize, message: Arc<str> },
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            rules: Vec::new(),
            default_decision: PolicyVerb::Deny,
        }
    }
}

impl Policy {
    pub fn allow_tools(tools: impl IntoIterator<Item = impl Into<Arc<str>>>) -> Self {
        Self {
            rules: tools
                .into_iter()
                .map(|tool| PolicyRule {
                    action: PolicyAction::Tool(tool.into()),
                    decision: PolicyVerb::Allow,
                    reason: None,
                    arg_equals: BTreeMap::new(),
                })
                .collect(),
            default_decision: PolicyVerb::Deny,
        }
    }

    pub fn ask_user_tools(tools: impl IntoIterator<Item = impl Into<Arc<str>>>) -> Self {
        Self {
            rules: tools
                .into_iter()
                .map(|tool| PolicyRule {
                    action: PolicyAction::Tool(tool.into()),
                    decision: PolicyVerb::AskUser,
                    reason: Some(Arc::from("tool requires approval")),
                    arg_equals: BTreeMap::new(),
                })
                .collect(),
            default_decision: PolicyVerb::Deny,
        }
    }

    pub fn phase3_reference() -> Self {
        Self {
            rules: vec![
                PolicyRule {
                    action: PolicyAction::Tool(Arc::from("shell")),
                    decision: PolicyVerb::AskUser,
                    reason: Some(Arc::from("shell tool requires user approval")),
                    arg_equals: BTreeMap::new(),
                },
                PolicyRule {
                    action: PolicyAction::Tool(Arc::from("file")),
                    decision: PolicyVerb::Allow,
                    reason: None,
                    arg_equals: BTreeMap::from([(Arc::from("operation"), Value::from("read"))]),
                },
            ],
            default_decision: PolicyVerb::Deny,
        }
    }

    pub fn phase4_reference() -> Self {
        let mut policy = Self::phase3_reference();
        policy.rules.extend(memory_policy_rules());
        policy
    }

    pub fn from_yaml(input: &str) -> Result<Self, PolicyError> {
        parse_policy_yaml(input)
    }

    pub fn decide(&self, plan: &Plan) -> PolicyDecision {
        if matches!(plan, Plan::Reply(_)) {
            return PolicyDecision::Allow;
        }

        let tool_args = match plan {
            Plan::CallTool(call) => serde_json::from_str::<Value>(call.args.get()).ok(),
            Plan::Reply(_) | Plan::Handoff(_, _) | Plan::Delegate(_) | Plan::Escalate(_) => None,
        };

        for rule in &self.rules {
            if rule.matches(plan, tool_args.as_ref()) {
                return rule.to_decision();
            }
        }

        default_policy_decision(&self.default_decision, plan)
    }

    pub fn narrow(parent: &Self, child: &Self) -> Result<Self, PolicyError> {
        if !default_decision_covers(&parent.default_decision, &child.default_decision) {
            return Err(PolicyError::Widened(Arc::from("default")));
        }

        for child_rule in &child.rules {
            match child_rule.decision {
                PolicyVerb::Allow => {
                    if !parent.rules.iter().any(|parent_rule| {
                        parent_rule.decision == PolicyVerb::Allow && parent_rule.covers(child_rule)
                    }) {
                        return Err(PolicyError::Widened(child_rule.label()));
                    }
                }
                PolicyVerb::AskUser => {
                    if !parent.rules.iter().any(|parent_rule| {
                        matches!(
                            parent_rule.decision,
                            PolicyVerb::Allow | PolicyVerb::AskUser
                        ) && parent_rule.covers(child_rule)
                    }) {
                        return Err(PolicyError::Widened(child_rule.label()));
                    }
                }
                PolicyVerb::Deny => {}
            }
        }

        Ok(child.clone())
    }
}

fn memory_policy_rules() -> Vec<PolicyRule> {
    vec![
        PolicyRule {
            action: PolicyAction::Tool(Arc::from("memory")),
            decision: PolicyVerb::Allow,
            reason: None,
            arg_equals: BTreeMap::from([(Arc::from("operation"), Value::from("read"))]),
        },
        PolicyRule {
            action: PolicyAction::Tool(Arc::from("memory")),
            decision: PolicyVerb::AskUser,
            reason: Some(Arc::from("memory write requires user approval")),
            arg_equals: BTreeMap::from([(Arc::from("operation"), Value::from("write"))]),
        },
        PolicyRule {
            action: PolicyAction::Tool(Arc::from("memory")),
            decision: PolicyVerb::AskUser,
            reason: Some(Arc::from("memory forget requires user approval")),
            arg_equals: BTreeMap::from([(Arc::from("operation"), Value::from("forget"))]),
        },
    ]
}

fn default_decision_covers(parent: &PolicyVerb, child: &PolicyVerb) -> bool {
    match child {
        PolicyVerb::Deny => true,
        PolicyVerb::AskUser => matches!(parent, PolicyVerb::Allow | PolicyVerb::AskUser),
        PolicyVerb::Allow => matches!(parent, PolicyVerb::Allow),
    }
}

impl PolicyRule {
    fn matches(&self, plan: &Plan, tool_args: Option<&Value>) -> bool {
        match (&self.action, plan) {
            (PolicyAction::Any, _) => self.args_match(tool_args),
            (PolicyAction::Tool(expected), Plan::CallTool(call)) if expected == &call.name => {
                self.args_match(tool_args)
            }
            (PolicyAction::Handoff, Plan::Handoff(_, _)) => true,
            (PolicyAction::Delegate, Plan::Delegate(_)) => true,
            (PolicyAction::Escalate, Plan::Escalate(_)) => true,
            (PolicyAction::Tool(_), _)
            | (PolicyAction::Handoff, _)
            | (PolicyAction::Delegate, _)
            | (PolicyAction::Escalate, _) => false,
        }
    }

    fn args_match(&self, tool_args: Option<&Value>) -> bool {
        if self.arg_equals.is_empty() {
            return true;
        }

        let Some(Value::Object(args)) = tool_args else {
            return false;
        };
        self.arg_equals
            .iter()
            .all(|(key, expected)| args.get(key.as_ref()) == Some(expected))
    }

    fn to_decision(&self) -> PolicyDecision {
        match self.decision {
            PolicyVerb::Allow => PolicyDecision::Allow,
            PolicyVerb::Deny => PolicyDecision::Deny {
                reason: self
                    .reason
                    .clone()
                    .unwrap_or_else(|| Arc::from("policy denied action")),
            },
            PolicyVerb::AskUser => PolicyDecision::AskUser {
                reason: self
                    .reason
                    .clone()
                    .unwrap_or_else(|| Arc::from("policy requires user approval")),
            },
        }
    }

    fn covers(&self, child: &Self) -> bool {
        match (&self.action, &child.action) {
            (PolicyAction::Any, _) => {}
            (PolicyAction::Tool(parent), PolicyAction::Tool(child)) if parent == child => {}
            (PolicyAction::Handoff, PolicyAction::Handoff)
            | (PolicyAction::Delegate, PolicyAction::Delegate)
            | (PolicyAction::Escalate, PolicyAction::Escalate) => {}
            _ => return false,
        }

        self.arg_equals
            .iter()
            .all(|(key, value)| child.arg_equals.get(key) == Some(value))
    }

    fn label(&self) -> Arc<str> {
        match &self.action {
            PolicyAction::Any => Arc::from("any"),
            PolicyAction::Tool(name) => Arc::clone(name),
            PolicyAction::Handoff => Arc::from("handoff"),
            PolicyAction::Delegate => Arc::from("delegate"),
            PolicyAction::Escalate => Arc::from("escalate"),
        }
    }
}

fn default_policy_decision(verb: &PolicyVerb, plan: &Plan) -> PolicyDecision {
    match verb {
        PolicyVerb::Allow => PolicyDecision::Allow,
        PolicyVerb::Deny => PolicyDecision::Deny {
            reason: Arc::from(default_deny_reason(plan)),
        },
        PolicyVerb::AskUser => PolicyDecision::AskUser {
            reason: Arc::from("policy requires user approval"),
        },
    }
}

fn default_deny_reason(plan: &Plan) -> String {
    match plan {
        Plan::Reply(_) => "reply is allowed".to_owned(),
        Plan::CallTool(call) => format!("tool '{}' is not allowed", call.name),
        Plan::Handoff(agent_id, _) => format!("handoff to '{}' is not allowed", agent_id.as_str()),
        Plan::Delegate(spec) => {
            format!("delegation to '{}' is not allowed", spec.agent_id.as_str())
        }
        Plan::Escalate(spec) => {
            format!("escalation to '{}' is not allowed", spec.template.name)
        }
    }
}

fn parse_policy_yaml(input: &str) -> Result<Policy, PolicyError> {
    let mut policy = Policy::default();
    let mut current_rule: Option<PolicyRule> = None;
    let mut in_args = false;

    for (index, raw_line) in input.lines().enumerate() {
        let line_number = index + 1;
        let trimmed = strip_comment(raw_line).trim();
        if trimmed.is_empty() || trimmed == "rules:" {
            continue;
        }

        if let Some(value) = trimmed.strip_prefix("default:") {
            policy.default_decision = parse_verb(value.trim(), line_number)?;
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("- ") {
            if let Some(rule) = current_rule.take() {
                policy.rules.push(rule);
            }
            current_rule = Some(PolicyRule {
                action: PolicyAction::Any,
                decision: PolicyVerb::Deny,
                reason: None,
                arg_equals: BTreeMap::new(),
            });
            in_args = false;
            apply_rule_field(
                current_rule.as_mut().expect("rule was just initialized"),
                rest,
                line_number,
                &mut in_args,
            )?;
            continue;
        }

        let Some(rule) = current_rule.as_mut() else {
            return Err(invalid_yaml(line_number, "field is outside of a rule"));
        };

        if in_args && !trimmed.contains(':') {
            return Err(invalid_yaml(line_number, "argument matcher is missing ':'"));
        }
        apply_rule_field(rule, trimmed, line_number, &mut in_args)?;
    }

    if let Some(rule) = current_rule.take() {
        policy.rules.push(rule);
    }

    Ok(policy)
}

fn apply_rule_field(
    rule: &mut PolicyRule,
    field: &str,
    line_number: usize,
    in_args: &mut bool,
) -> Result<(), PolicyError> {
    if field == "args:" {
        *in_args = true;
        return Ok(());
    }

    let (key, value) = field
        .split_once(':')
        .ok_or_else(|| invalid_yaml(line_number, "rule field is missing ':'"))?;
    let key = key.trim();
    let value = value.trim();

    if *in_args && !matches!(key, "tool" | "action" | "decision" | "reason") {
        rule.arg_equals.insert(Arc::from(key), parse_scalar(value));
        return Ok(());
    }
    *in_args = false;

    match key {
        "tool" => {
            rule.action = PolicyAction::Tool(unquote(value));
            Ok(())
        }
        "action" => {
            rule.action = match unquote(value).as_ref() {
                "any" => PolicyAction::Any,
                "handoff" => PolicyAction::Handoff,
                "delegate" => PolicyAction::Delegate,
                "escalate" => PolicyAction::Escalate,
                other => PolicyAction::Tool(Arc::from(other)),
            };
            Ok(())
        }
        "decision" => {
            rule.decision = parse_verb(value, line_number)?;
            Ok(())
        }
        "reason" => {
            rule.reason = Some(unquote(value));
            Ok(())
        }
        other => Err(invalid_yaml(
            line_number,
            format!("unknown rule field '{other}'"),
        )),
    }
}

fn parse_verb(value: &str, line: usize) -> Result<PolicyVerb, PolicyError> {
    match unquote(value).as_ref() {
        "allow" => Ok(PolicyVerb::Allow),
        "deny" => Ok(PolicyVerb::Deny),
        "ask_user" => Ok(PolicyVerb::AskUser),
        other => Err(invalid_yaml(line, format!("unknown policy verb '{other}'"))),
    }
}

fn parse_scalar(value: &str) -> Value {
    let value = unquote(value);
    match value.as_ref() {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        "null" => Value::Null,
        _ => value
            .parse::<i64>()
            .map(Value::from)
            .unwrap_or_else(|_| Value::String(value.to_string())),
    }
}

fn strip_comment(line: &str) -> &str {
    line.split_once('#').map_or(line, |(before, _)| before)
}

fn unquote(value: &str) -> Arc<str> {
    let value = value.trim();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        Arc::from(&value[1..value.len() - 1])
    } else {
        Arc::from(value)
    }
}

fn invalid_yaml(line: usize, message: impl Into<Arc<str>>) -> PolicyError {
    PolicyError::InvalidYaml {
        line,
        message: message.into(),
    }
}

pub fn tool_call_approval_id(call: &ToolCall) -> Arc<str> {
    Arc::from(format!("approval-{}", call.id.as_str()))
}
