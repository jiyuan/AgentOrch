//! Deterministic planner for the `skill-creator` workspace skill.
//!
//! Detects skill creation requests and keeps the framework-side validation
//! loop deterministic. The LLM generates skill content through the `file`
//! tool; the planner follows completed `SKILL.md` writes with
//! `skill_validate`, then returns validator output to the LLM so it can
//! finish bundle completeness checks before responding.

use super::{SkillPlanner, WorkspaceSkillCatalog};
use agentos_interfaces::orchestrator::{OrchestratorError, Plan, RunContext};
use agentos_proto::{Message, MessageRole, ToolCall, ToolCallId, ToolStatus};
use serde::Deserialize;
use serde_json::{json, value::RawValue};
use std::path::PathBuf;
use std::sync::Arc;

pub struct SkillCreatorSkill {
    catalog: WorkspaceSkillCatalog,
}

impl SkillCreatorSkill {
    pub fn new(catalog: WorkspaceSkillCatalog) -> Self {
        Self { catalog }
    }
}

impl SkillPlanner for SkillCreatorSkill {
    fn name(&self) -> &str {
        "skill-creator"
    }

    fn plan(&self, ctx: &RunContext<'_>) -> Result<Option<Plan>, OrchestratorError> {
        if !self.catalog.contains("skill-creator") {
            return Ok(None);
        }
        let Some(item) = ctx.state.transcript.items.last() else {
            return Ok(None);
        };
        match item.message.role {
            MessageRole::User => plan_user_request(&item.message.content),
            MessageRole::Tool => plan_tool_followup(ctx),
            MessageRole::Assistant | MessageRole::System => Ok(None),
        }
    }
}

fn plan_user_request(input: &str) -> Result<Option<Plan>, OrchestratorError> {
    if !is_skill_authoring_request(input) {
        return Ok(None);
    }
    if is_skill_update_request(input) {
        // Updating an existing skill: the model owns the edit. This planner
        // still owns the deterministic `skill_validate` follow-up after the
        // model writes the file (see `plan_tool_followup`), so there is no
        // "please provide a name" prompt here — the target skill already
        // exists and is named in the request.
        return Ok(None);
    }
    if parse_create_request(input).is_some() {
        // Let MaxOrchestrator route this to the tool-aware LLM path. The
        // model owns generation; this planner owns deterministic validation
        // follow-up after the model writes SKILL.md.
        return Ok(None);
    }
    Ok(Some(Plan::Reply(Message::text(
        MessageRole::Assistant,
        "Please provide a hyphen-case skill name, for example: create skill: audit-skill, audit repository changes and report risks.",
    ))))
}

/// Strip a leading politeness marker (`please`, `can you`, `could you`) so the
/// verb detection below sees the actual request.
fn strip_politeness(lower: &str) -> &str {
    lower
        .strip_prefix("please ")
        .or_else(|| lower.strip_prefix("can you "))
        .or_else(|| lower.strip_prefix("could you "))
        .unwrap_or(lower)
}

pub(crate) fn is_skill_creation_request(input: &str) -> bool {
    let lower = input.trim_start().to_ascii_lowercase();
    let lower = strip_politeness(&lower);
    has_create_prefix(lower) || (lower.contains("create") && lower.contains("skill"))
}

/// True when the request asks to change an *existing* skill (update, improve,
/// fix a validation failure, etc.). These must stay on the local
/// skill-creator path: the skill's documented contract is "Create, update,
/// and validate AgentOS workspace skills", and the deterministic
/// write -> validate -> repair follow-up only runs when the skill-creator
/// planner is engaged. Without this, "update the audit-skill skill" is
/// routed to a generic code-fix sub-agent that has no skill-creator workflow
/// and exhausts its turn budget on a manual validate/repair loop.
pub(crate) fn is_skill_update_request(input: &str) -> bool {
    let lower = input.trim_start().to_ascii_lowercase();
    let lower = strip_politeness(&lower);
    if !lower.contains("skill") {
        return false;
    }
    const UPDATE_VERBS: [&str; 11] = [
        "update", "improve", "edit", "modify", "revise", "rewrite", "fix", "tune", "rebuild",
        "refine", "refactor",
    ];
    UPDATE_VERBS.iter().any(|verb| lower.contains(verb))
}

/// A skill *authoring* request is either creating a new skill or changing an
/// existing one. Both belong on the local skill-creator path.
pub(crate) fn is_skill_authoring_request(input: &str) -> bool {
    is_skill_creation_request(input) || is_skill_update_request(input)
}

fn plan_tool_followup(ctx: &RunContext<'_>) -> Result<Option<Plan>, OrchestratorError> {
    let Some(completed) = latest_completed_tool_call(ctx) else {
        return Ok(None);
    };
    match completed.call.name.as_ref() {
        "file" => plan_validate_after_write(&completed),
        "skill_validate" => plan_after_validation(&completed),
        _ => Ok(None),
    }
}

fn title_from_name(name: &str) -> String {
    name.split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, PartialEq)]
struct ParsedCreate {
    name: String,
    description: String,
}

fn parse_create_request(input: &str) -> Option<ParsedCreate> {
    parse_create_prefix(input).or_else(|| parse_natural_create(input))
}

fn parse_create_prefix(input: &str) -> Option<ParsedCreate> {
    let trimmed = input.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    let prefix = create_prefix(&lower)?;
    let body = trimmed[prefix.len()..].trim();
    let (name, description) = parse_name_and_description(body)?;
    Some(ParsedCreate { name, description })
}

fn parse_natural_create(input: &str) -> Option<ParsedCreate> {
    if !is_skill_creation_request(input) {
        return None;
    }
    let trimmed = input.trim();
    let (raw_name, tail) = extract_quoted_name(trimmed)
        .or_else(|| extract_name_after_marker(trimmed, "called "))
        .or_else(|| extract_name_after_marker(trimmed, "named "))
        .or_else(|| extract_hyphenated_name(trimmed))?;
    let name = normalize_skill_name(&raw_name)?;
    let description = description_from_tail(&name, tail);
    Some(ParsedCreate { name, description })
}

struct CompletedToolCall<'a> {
    call: &'a ToolCall,
    status: Option<ToolStatus>,
    content: &'a str,
}

#[derive(Deserialize)]
struct FileCallArgs {
    operation: String,
    path: PathBuf,
}

#[derive(Deserialize)]
struct SkillValidateCallArgs {
    name: String,
}

fn latest_completed_tool_call<'a>(ctx: &'a RunContext<'_>) -> Option<CompletedToolCall<'a>> {
    let latest = ctx.state.transcript.items.last()?;
    if latest.message.role != MessageRole::Tool {
        return None;
    }
    let call_id = latest.message.tool_call_id.as_ref()?;
    let call = ctx
        .state
        .transcript
        .items
        .iter()
        .rev()
        .skip(1)
        .filter(|item| item.message.role == MessageRole::Assistant)
        .flat_map(|item| item.message.tool_calls.iter())
        .find(|call| &call.id == call_id)?;
    Some(CompletedToolCall {
        call,
        status: tool_status(&latest.message),
        content: latest.message.content.as_ref(),
    })
}

fn plan_validate_after_write(
    completed: &CompletedToolCall<'_>,
) -> Result<Option<Plan>, OrchestratorError> {
    if completed.status != Some(ToolStatus::Succeeded) {
        return Ok(None);
    }
    let Some(skill_name) = skill_name_from_file_write(completed.call) else {
        return Ok(None);
    };
    let args = json!({ "name": skill_name });
    let raw_args = RawValue::from_string(args.to_string())
        .map_err(|err| OrchestratorError::Backend(err.to_string().into()))?;
    Ok(Some(Plan::CallTool(ToolCall {
        id: ToolCallId::new(format!("skill-creator-validate-{skill_name}")),
        name: Arc::from("skill_validate"),
        args: raw_args,
    })))
}

fn plan_after_validation(
    completed: &CompletedToolCall<'_>,
) -> Result<Option<Plan>, OrchestratorError> {
    let Some(skill_name) = skill_name_from_validate_call(completed.call) else {
        return Ok(None);
    };
    if completed.status == Some(ToolStatus::Succeeded) && completed.content.contains("PASS") {
        // A validation PASS means SKILL.md is structurally valid, not that
        // the full generated bundle is complete. Return control to the
        // tool-aware LLM path so the model can inspect the validator's
        // inventory, write any missing scripts/references/assets, and only
        // then produce the final user-facing summary.
        let _ = skill_name;
        return Ok(None);
    }
    // Validation failures are recoverable. Return None so MaxOrchestrator can
    // send the failed tool result back through the LLM tool path; the model
    // remains responsible for generating the corrected SKILL.md.
    Ok(None)
}

fn skill_name_from_file_write(call: &ToolCall) -> Option<String> {
    let args = serde_json::from_str::<FileCallArgs>(call.args.get()).ok()?;
    if args.operation != "write" || args.path.file_name()? != "SKILL.md" {
        return None;
    }
    let raw_name = args.path.parent()?.file_name()?.to_str()?;
    normalize_skill_name(raw_name)
}

fn skill_name_from_validate_call(call: &ToolCall) -> Option<String> {
    let args = serde_json::from_str::<SkillValidateCallArgs>(call.args.get()).ok()?;
    normalize_skill_name(&args.name)
}

fn tool_status(message: &Message) -> Option<ToolStatus> {
    let value = message.metadata.get("tool_status")?.as_str()?;
    match value {
        "succeeded" => Some(ToolStatus::Succeeded),
        "failed" => Some(ToolStatus::Failed),
        "denied" => Some(ToolStatus::Denied),
        _ => None,
    }
}

fn create_prefix(input: &str) -> Option<&'static str> {
    if input.starts_with("create skill:") {
        Some("create skill:")
    } else if input.starts_with("skill create:") {
        Some("skill create:")
    } else {
        None
    }
}

fn has_create_prefix(input: &str) -> bool {
    create_prefix(input).is_some()
        || input.starts_with("create a skill")
        || input.starts_with("create skill ")
        || input.starts_with("make a skill")
        || input.starts_with("build a skill")
        || input.starts_with("add a skill")
}

fn parse_name_and_description(body: &str) -> Option<(String, String)> {
    let mut parts = body.splitn(2, ',').map(str::trim);
    let name = normalize_skill_name(parts.next()?)?;
    let description = parts
        .next()
        .filter(|description| !description.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_description(&name));
    Some((name, description))
}

fn extract_quoted_name(input: &str) -> Option<(String, &str)> {
    for quote in ['\'', '"', '`'] {
        let Some(start) = input.find(quote) else {
            continue;
        };
        let after_start = &input[start + quote.len_utf8()..];
        let Some(end) = after_start.find(quote) else {
            continue;
        };
        let raw_name = after_start[..end].trim();
        if raw_name.is_empty() {
            continue;
        }
        let tail = &after_start[end + quote.len_utf8()..];
        return Some((raw_name.to_owned(), tail));
    }
    None
}

fn extract_name_after_marker<'a>(input: &'a str, marker: &str) -> Option<(String, &'a str)> {
    let lower = input.to_ascii_lowercase();
    let start = lower.find(marker)? + marker.len();
    let rest = input[start..].trim_start();
    let mut end = rest.len();
    for separator in [",", ".", " for ", " to ", " that ", " which "] {
        if let Some(index) = rest.to_ascii_lowercase().find(separator) {
            end = end.min(index);
        }
    }
    let raw_name = rest[..end].trim();
    if raw_name.is_empty() {
        return None;
    }
    Some((raw_name.to_owned(), &rest[end..]))
}

fn extract_hyphenated_name(input: &str) -> Option<(String, &str)> {
    let start = input
        .char_indices()
        .find(|(_, ch)| ch.is_ascii_alphanumeric())
        .map(|(index, _)| index)?;
    for (offset, token) in input[start..].split_whitespace().enumerate() {
        let cleaned = token.trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-');
        if cleaned.contains('-') && normalize_skill_name(cleaned).is_some() {
            let token_start = input.find(token)?;
            let tail = &input[token_start + token.len()..];
            return Some((cleaned.to_owned(), tail));
        }
        if offset > 12 {
            break;
        }
    }
    None
}

fn description_from_tail(name: &str, tail: &str) -> String {
    let lower = tail.to_ascii_lowercase();
    let Some(index) = lower.find(" for ") else {
        return default_description(name);
    };
    let purpose = tail[index + " for ".len()..]
        .trim()
        .trim_end_matches(['.', '?', '!'])
        .trim();
    if purpose.is_empty() {
        default_description(name)
    } else {
        format!("Skill for {purpose}.")
    }
}

fn default_description(name: &str) -> String {
    format!("Support the {} workflow.", title_from_name(name))
}

fn normalize_skill_name(raw: &str) -> Option<String> {
    let mut normalized = String::new();
    let mut last_was_dash = false;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if (ch == '-' || ch == '_' || ch.is_ascii_whitespace()) && !last_was_dash {
            normalized.push('-');
            last_was_dash = true;
        }
    }
    let normalized = normalized.trim_matches('-').to_owned();
    (!normalized.is_empty()).then_some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_interfaces::session::Item;
    use agentos_interfaces::RunState;
    use agentos_proto::{AgentId, RunId};
    use serde_json::Value;
    use std::collections::BTreeMap;

    #[test]
    fn parses_name_and_description() {
        let parsed = parse_create_request("create skill: rss-digest, fetch and summarize feeds")
            .expect("should parse");
        assert_eq!(parsed.name, "rss-digest");
        assert_eq!(parsed.description, "fetch and summarize feeds");
    }

    #[test]
    fn accepts_skill_create_synonym() {
        let parsed =
            parse_create_request("skill create: foo, bar baz").expect("synonym should parse");
        assert_eq!(parsed.name, "foo");
    }

    #[test]
    fn parses_prefix_case_insensitively() {
        let parsed =
            parse_create_request("  Create Skill: rss-digest, fetch feeds").expect("should parse");
        assert_eq!(parsed.name, "rss-digest");
        assert_eq!(parsed.description, "fetch feeds");
    }

    #[test]
    fn parses_quoted_natural_language_name() {
        let parsed = parse_create_request("create a 'audit-skill'").expect("should parse");
        assert_eq!(parsed.name, "audit-skill");
        assert_eq!(parsed.description, "Support the Audit Skill workflow.");
    }

    #[test]
    fn parses_called_name_with_purpose() {
        let parsed =
            parse_create_request("please create a skill called RSS Digest for summarizing feeds")
                .expect("should parse");
        assert_eq!(parsed.name, "rss-digest");
        assert_eq!(parsed.description, "Skill for summarizing feeds.");
    }

    #[test]
    fn detects_natural_language_skill_creation_requests() {
        assert!(is_skill_creation_request(
            "please create a skill called rss-digest"
        ));
        assert!(is_skill_creation_request(
            "Can you create a skill for summarizing feeds?"
        ));
        assert!(!is_skill_creation_request("please install a skill"));
        assert!(!is_skill_creation_request("write me a haiku"));
    }

    #[test]
    fn detects_skill_update_requests() {
        assert!(is_skill_update_request("update the audit-skill skill"));
        assert!(is_skill_update_request(
            "please improve my logger skill so it appends a timestamp"
        ));
        assert!(is_skill_update_request("fix the skill validation failure"));
        assert!(is_skill_update_request("rewrite the web-research skill"));
        // Not skill work: must mention a skill.
        assert!(!is_skill_update_request("fix the failing test"));
        assert!(!is_skill_update_request("update the dependencies"));
        // Creation is handled by the creation predicate, not this one.
        assert!(!is_skill_update_request("create a skill called rss-digest"));
    }

    #[test]
    fn authoring_covers_both_create_and_update() {
        assert!(is_skill_authoring_request("create a skill called logger"));
        assert!(is_skill_authoring_request("update the logger skill"));
        assert!(!is_skill_authoring_request("summarize this document"));
    }

    #[test]
    fn update_request_planner_defers_to_model_without_name_prompt() {
        // An update request must not emit the "please provide a name" reply;
        // it should abstain (Ok(None)) so the model performs the edit and the
        // deterministic validate follow-up runs after the file write.
        let plan = plan_user_request("please update the audit-skill skill to add a section")
            .expect("planner ok");
        assert!(plan.is_none());
    }

    #[test]
    fn defaults_missing_description() {
        let parsed = parse_create_request("create skill: rss-digest").expect("should parse");
        assert_eq!(parsed.description, "Support the Rss Digest workflow.");
    }

    #[test]
    fn rejects_unrelated_prompts() {
        assert!(parse_create_request("write me a haiku").is_none());
        assert!(parse_create_request("please install a skill").is_none());
    }

    #[test]
    fn valid_skill_creation_request_falls_through_to_llm_generation() {
        assert!(
            plan_user_request("create a 'audit-skill'")
                .expect("plan should succeed")
                .is_none(),
            "the LLM should generate skill content"
        );
    }

    #[test]
    fn skill_creation_without_name_requests_clarification() {
        let plan = plan_user_request("please create a skill")
            .expect("plan should succeed")
            .expect("clarification expected");
        let Plan::Reply(message) = plan else {
            panic!("expected reply");
        };
        assert!(message.content.contains("hyphen-case skill name"));
    }

    #[test]
    fn file_write_success_plans_validation() {
        let call = tool_call(
            "skill-creator-write-audit-skill",
            "file",
            json!({
                "operation": "write",
                "path": "skills/audit-skill/SKILL.md",
                "content": "content",
            }),
        );
        let state = state_after_tool(call, ToolStatus::Succeeded, "wrote 7 bytes");
        let ctx = RunContext::from_state(&state);

        let plan = plan_tool_followup(&ctx)
            .expect("planner should succeed")
            .expect("validation plan expected");
        let Plan::CallTool(call) = plan else {
            panic!("expected validation tool call");
        };
        assert_eq!(call.name.as_ref(), "skill_validate");
        assert!(call.args.get().contains(r#""name":"audit-skill""#));
    }

    #[test]
    fn validation_success_returns_to_llm_for_bundle_completion() {
        let call = tool_call(
            "skill-creator-validate-audit-skill",
            "skill_validate",
            json!({ "name": "audit-skill" }),
        );
        let state = state_after_tool(
            call,
            ToolStatus::Succeeded,
            "skill_validate: PASS - 'audit-skill'",
        );
        let ctx = RunContext::from_state(&state);

        assert!(
            plan_tool_followup(&ctx)
                .expect("planner should succeed")
                .is_none(),
            "validation pass should return to the LLM for bundle inventory review"
        );
    }

    #[test]
    fn validation_failure_falls_through_for_llm_repair() {
        let call = tool_call(
            "skill-creator-validate-audit-skill",
            "skill_validate",
            json!({ "name": "audit-skill" }),
        );
        let state = state_after_tool(call, ToolStatus::Failed, "skill_validate: FAIL - no body");
        let ctx = RunContext::from_state(&state);

        assert!(
            plan_tool_followup(&ctx)
                .expect("planner should succeed")
                .is_none(),
            "validation failure should return to the LLM repair path"
        );
    }

    fn tool_call(id: &str, name: &str, args: Value) -> ToolCall {
        ToolCall {
            id: ToolCallId::new(id),
            name: Arc::from(name),
            args: RawValue::from_string(args.to_string()).expect("raw args"),
        }
    }

    fn state_after_tool(call: ToolCall, status: ToolStatus, content: &str) -> RunState {
        let mut state = RunState::new(RunId::new("run-skill"), AgentId::new("default"));
        state.transcript.items.push(Item {
            message: Message::text(MessageRole::User, "create a 'audit-skill'"),
            metadata: BTreeMap::new(),
        });
        state.transcript.items.push(Item {
            message: Message {
                role: MessageRole::Assistant,
                content: Arc::from(""),
                attachments: Vec::new(),
                tool_calls: vec![call.clone()],
                tool_call_id: None,
                metadata: BTreeMap::new(),
            },
            metadata: BTreeMap::new(),
        });
        let mut metadata = BTreeMap::new();
        metadata.insert(
            Arc::from("tool_status"),
            Value::String(
                match status {
                    ToolStatus::Succeeded => "succeeded",
                    ToolStatus::Failed => "failed",
                    ToolStatus::Denied => "denied",
                }
                .to_owned(),
            ),
        );
        state.transcript.items.push(Item {
            message: Message {
                role: MessageRole::Tool,
                content: Arc::from(content),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: Some(call.id),
                metadata,
            },
            metadata: BTreeMap::new(),
        });
        state
    }
}
