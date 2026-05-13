//! Deterministic planner for the `skill-creator` workspace skill.
//!
//! Detects compact "create skill: NAME, DESCRIPTION" prefixes in the latest
//! user message and emits a `Plan::CallTool` against the `file` tool that
//! scaffolds a valid `SKILL.md` under `workspace/skills/<name>/`. From there
//! the LLM is expected to populate the body (via the `file` tool) and call
//! `skill_validate` to confirm. Natural-language requests don't match this
//! prefix and are handled entirely by the LLM through the same two tools.

use super::{SkillPlanner, WorkspaceSkillCatalog};
use agentos_interfaces::orchestrator::{OrchestratorError, Plan, RunContext};
use agentos_proto::{Message, MessageRole, ToolCall, ToolCallId};
use serde_json::{json, value::RawValue};
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
            MessageRole::User => plan_scaffold(&item.message.content),
            MessageRole::Tool => plan_acknowledgement(ctx),
            MessageRole::Assistant | MessageRole::System => Ok(None),
        }
    }
}

fn plan_scaffold(input: &str) -> Result<Option<Plan>, OrchestratorError> {
    let Some(parsed) = parse_create_prefix(input) else {
        return Ok(None);
    };
    // Emit a `file` write that lays down a valid `SKILL.md` skeleton for
    // the requested name. The LLM then sees the tool result and is expected
    // to follow up: write any additional body content / bundled resources
    // via the same `file` tool, then call `skill_validate` to confirm the
    // result parses. Tool = validator, LLM = generator — keep them separate.
    let frontmatter = format!(
        "---\nname: {}\ndescription: {}\n---\n\n# {}\n\n*Body intentionally minimal — fill in with the workflow this skill standardises, then call `skill_validate`.*\n",
        parsed.name,
        parsed.description,
        title_from_name(&parsed.name),
    );
    let args = json!({
        "operation": "write",
        "path": format!("workspace/skills/{}/SKILL.md", parsed.name),
        "content": frontmatter,
    });
    let raw_args = RawValue::from_string(args.to_string())
        .map_err(|err| OrchestratorError::Backend(err.to_string().into()))?;
    Ok(Some(Plan::CallTool(ToolCall {
        id: ToolCallId::new(format!("skill-creator-{}", parsed.name)),
        name: Arc::from("file"),
        args: raw_args,
    })))
}

fn plan_acknowledgement(ctx: &RunContext<'_>) -> Result<Option<Plan>, OrchestratorError> {
    if !previous_user_requested_skill_create(ctx) {
        return Ok(None);
    }
    let Some(item) = ctx.state.transcript.items.last() else {
        return Ok(None);
    };
    // Echo the `file` tool's success message back as the assistant reply so
    // the prefix shortcut completes without a second LLM round-trip. The
    // user then drives the rest of the workflow (populate the body via
    // `file`, run `skill_validate`) through natural-language follow-ups.
    Ok(Some(Plan::Reply(Message::text(
        MessageRole::Assistant,
        Arc::clone(&item.message.content),
    ))))
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

fn parse_create_prefix(input: &str) -> Option<ParsedCreate> {
    let body = input
        .strip_prefix("create skill:")
        .or_else(|| input.strip_prefix("skill create:"))?
        .trim();
    let mut parts = body.splitn(2, ',').map(str::trim).collect::<Vec<_>>();
    let name = parts.first()?.to_string();
    if name.is_empty() {
        return None;
    }
    let description = parts.get(1).map(|s| s.to_string()).unwrap_or_default();
    if description.is_empty() {
        return None;
    }
    parts.clear();
    Some(ParsedCreate { name, description })
}

fn previous_user_requested_skill_create(ctx: &RunContext<'_>) -> bool {
    ctx.state.transcript.items.iter().rev().skip(1).any(|item| {
        if item.message.role != MessageRole::User {
            return false;
        }
        let lower = item.message.content.to_ascii_lowercase();
        lower.starts_with("create skill:") || lower.starts_with("skill create:")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_name_and_description() {
        let parsed = parse_create_prefix("create skill: rss-digest, fetch and summarize feeds")
            .expect("should parse");
        assert_eq!(parsed.name, "rss-digest");
        assert_eq!(parsed.description, "fetch and summarize feeds");
    }

    #[test]
    fn accepts_skill_create_synonym() {
        let parsed =
            parse_create_prefix("skill create: foo, bar baz").expect("synonym should parse");
        assert_eq!(parsed.name, "foo");
    }

    #[test]
    fn rejects_missing_description() {
        assert!(parse_create_prefix("create skill: rss-digest").is_none());
    }

    #[test]
    fn rejects_unrelated_prompts() {
        assert!(parse_create_prefix("write me a haiku").is_none());
        assert!(parse_create_prefix("please install a skill").is_none());
    }
}
