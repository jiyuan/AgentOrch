use super::common::{default_skills_dir, elapsed_ms, result_metadata, skills_root_for_tests};
use crate::skills::{validate_skill_dir, SkillStoreError};
use agentos_interfaces::tool::{Tool, ToolError, ToolSpec};
use agentos_proto::{ToolCall, ToolResult, ToolStatus};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, value::RawValue};
use std::sync::Arc;
use std::time::Instant;

/// Validates that a workspace skill on disk parses as a valid SKILL.md
/// bundle. The LLM writes skill content via the `file` tool, then calls
/// this tool to confirm correctness. On failure, the result content is a
/// structured error message naming what's wrong so the LLM can fix it
/// and re-validate. This is the "validator" half of the LLM-generator /
/// tool-validator architecture — generation is owned by the model, not
/// the tool.
#[derive(Default)]
pub struct SkillValidateTool;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillValidateArgs {
    name: String,
}

#[async_trait]
impl Tool for SkillValidateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: Arc::from("skill_validate"),
            description: Arc::from(
                "Validate a workspace skill folder. Pass the skill `name` \
                 (lowercase hyphen-case) — the tool resolves it to \
                 `<skills_root>/<name>/` and checks the SKILL.md shape: \
                 frontmatter present and well-formed, required `name` and \
                 `description` fields, folder name matches `name`, body is \
                 non-empty. On failure returns a clear error string \
                 explaining what's wrong; on success returns the resolved \
                 path and a summary of the parsed metadata. Use this after \
                 every batch of `file` writes that build up a skill bundle, \
                 and loop until validation passes — the tool is the ground \
                 truth for whether the skill is ready.",
            ),
            input_schema: json!({
                "type": "object",
                "required": ["name"],
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Skill folder name (lowercase hyphen-case, e.g. 'audit-skill'). Resolved against the configured skills root — do not include any path prefix."
                    }
                }
            }),
            requires_isolation: false,
        }
    }

    async fn call(&self, call: &ToolCall, args: &RawValue) -> Result<ToolResult, ToolError> {
        let parsed: SkillValidateArgs = serde_json::from_str(args.get())
            .map_err(|err| ToolError::Failed(err.to_string().into()))?;
        let start = Instant::now();
        let root = skills_root_for_tests().unwrap_or_else(default_skills_dir);
        let skill_dir = root.join(&parsed.name);

        tracing::info!(
            tool = "skill_validate",
            skill_name = parsed.name.as_str(),
            skill_dir = %skill_dir.display(),
            "skill_validate invoked"
        );

        let outcome = validate_skill_dir(&skill_dir);
        match outcome {
            Ok(skill) => {
                let canonical = skill_dir.canonicalize().unwrap_or(skill_dir.clone());
                let message = format!(
                    "skill_validate: PASS — '{}' at {} (description: \"{}\")",
                    skill.name,
                    canonical.display(),
                    skill.description
                );
                tracing::info!(
                    tool = "skill_validate",
                    skill_name = %skill.name,
                    canonical_path = %canonical.display(),
                    "skill_validate passed"
                );
                let bytes_out = message.len() as u64;
                Ok(ToolResult {
                    call_id: call.id.clone(),
                    status: ToolStatus::Succeeded,
                    content: Arc::from(message),
                    metadata: result_metadata(elapsed_ms(start), bytes_out),
                })
            }
            Err(err) => {
                let message = format_validation_failure(&parsed.name, &skill_dir, &err);
                tracing::warn!(
                    tool = "skill_validate",
                    skill_name = parsed.name.as_str(),
                    error = %err,
                    "skill_validate failed"
                );
                let bytes_out = message.len() as u64;
                // Failed-status, not Err: validation failure is an
                // expected, recoverable signal that the LLM is supposed
                // to act on (read the message, fix the file, re-validate).
                // Returning Err would abort the run with a tool error.
                Ok(ToolResult {
                    call_id: call.id.clone(),
                    status: ToolStatus::Failed,
                    content: Arc::from(message),
                    metadata: result_metadata(elapsed_ms(start), bytes_out),
                })
            }
        }
    }
}

fn format_validation_failure(
    name: &str,
    skill_dir: &std::path::Path,
    err: &SkillStoreError,
) -> String {
    let hint = match err {
        SkillStoreError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
            "The skill directory or SKILL.md does not exist. Use the `file` \
             tool to write the bundle first, then re-run `skill_validate`."
        }
        SkillStoreError::Invalid { message, .. } if message.contains("frontmatter") => {
            "SKILL.md must start with `---\\n` and have a matching `\\n---` \
             closing delimiter, with `name:` and `description:` keys inside. \
             Re-write SKILL.md via the `file` tool with valid frontmatter."
        }
        SkillStoreError::Invalid { message, .. } if message.contains("body") => {
            "SKILL.md needs a non-empty body after the frontmatter — write \
             the workflow instructions there via the `file` tool."
        }
        SkillStoreError::Invalid { message, .. } if message.contains("folder") => {
            "The directory name and the frontmatter `name:` value must match. \
             Either rename the folder or change the frontmatter `name` field."
        }
        SkillStoreError::Invalid { .. } => {
            "The bundle is structurally invalid. Read the error above and fix \
             the SKILL.md, then re-validate."
        }
        SkillStoreError::Io { .. } => {
            "A filesystem error occurred while reading the bundle. Confirm \
             the path exists and is readable."
        }
        SkillStoreError::Missing(_) | SkillStoreError::Exists(_) => {
            "Unexpected store error during validation."
        }
    };
    format!(
        "skill_validate: FAIL — '{}' at {}\n  reason: {}\n  hint: {}",
        name,
        skill_dir.display(),
        err,
        hint
    )
}

#[cfg(test)]
mod tests {
    use super::super::common::test_support::{tool_call, SkillsDirGuard};
    use super::*;

    #[tokio::test]
    async fn skill_validate_passes_for_well_formed_bundle() {
        let guard = SkillsDirGuard::new("skill-validate-pass");
        let skill_dir = guard.dir.join("good-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: good-skill\ndescription: A valid skill.\n---\n\n# Good Skill\n\nReal body content.\n",
        )
        .unwrap();

        let args = json!({ "name": "good-skill" });
        let raw = RawValue::from_string(args.to_string()).unwrap();
        let result = SkillValidateTool
            .call(&tool_call("skill_validate", "pass"), &raw)
            .await
            .unwrap();
        assert_eq!(result.status, ToolStatus::Succeeded);
        assert!(
            result.content.contains("PASS"),
            "expected PASS, got: {}",
            result.content
        );
        assert!(result.content.contains("good-skill"));
    }

    #[tokio::test]
    async fn skill_validate_reports_missing_directory_with_hint() {
        let _guard = SkillsDirGuard::new("skill-validate-missing");
        let args = json!({ "name": "ghost-skill" });
        let raw = RawValue::from_string(args.to_string()).unwrap();
        let result = SkillValidateTool
            .call(&tool_call("skill_validate", "missing"), &raw)
            .await
            .unwrap();
        assert_eq!(result.status, ToolStatus::Failed);
        assert!(
            result.content.contains("FAIL") && result.content.contains("does not exist"),
            "expected FAIL+hint, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn skill_validate_reports_malformed_frontmatter_with_hint() {
        let guard = SkillsDirGuard::new("skill-validate-frontmatter");
        let skill_dir = guard.dir.join("broken-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "no frontmatter at all\n\nbody body body.\n",
        )
        .unwrap();

        let args = json!({ "name": "broken-skill" });
        let raw = RawValue::from_string(args.to_string()).unwrap();
        let result = SkillValidateTool
            .call(&tool_call("skill_validate", "broken"), &raw)
            .await
            .unwrap();
        assert_eq!(result.status, ToolStatus::Failed);
        assert!(
            result.content.contains("FAIL") && result.content.contains("frontmatter"),
            "expected FAIL+frontmatter hint, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn skill_validate_reports_folder_name_mismatch_with_hint() {
        let guard = SkillsDirGuard::new("skill-validate-mismatch");
        let skill_dir = guard.dir.join("foo");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: bar\ndescription: Mismatch.\n---\n\nbody.\n",
        )
        .unwrap();

        let args = json!({ "name": "foo" });
        let raw = RawValue::from_string(args.to_string()).unwrap();
        let result = SkillValidateTool
            .call(&tool_call("skill_validate", "mm"), &raw)
            .await
            .unwrap();
        assert_eq!(result.status, ToolStatus::Failed);
        assert!(
            result.content.contains("FAIL") && result.content.contains("folder"),
            "expected FAIL+folder hint, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn skill_validate_reports_empty_body_with_hint() {
        let guard = SkillsDirGuard::new("skill-validate-empty-body");
        let skill_dir = guard.dir.join("hollow-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: hollow-skill\ndescription: No body.\n---\n\n   \n",
        )
        .unwrap();

        let args = json!({ "name": "hollow-skill" });
        let raw = RawValue::from_string(args.to_string()).unwrap();
        let result = SkillValidateTool
            .call(&tool_call("skill_validate", "empty"), &raw)
            .await
            .unwrap();
        assert_eq!(result.status, ToolStatus::Failed);
        assert!(
            result.content.contains("FAIL") && result.content.contains("body"),
            "expected FAIL+body hint, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn skill_validate_rejects_unknown_fields() {
        let _guard = SkillsDirGuard::new("skill-validate-deny");
        let args = json!({ "name": "x", "path": "/tmp" });
        let raw = RawValue::from_string(args.to_string()).unwrap();
        let err = SkillValidateTool
            .call(&tool_call("skill_validate", "deny"), &raw)
            .await
            .unwrap_err();
        let ToolError::Failed(msg) = err;
        assert!(msg.contains("unknown field"), "got: {msg}");
    }
}
