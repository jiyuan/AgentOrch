use crate::tools::{safe_workspace_path, skills_dir, workspace_root};
use agentos_interfaces::guardrail::{
    GuardrailError, GuardrailOutcome, Input, InputGuardrail, OutputGuardrail, ToolGuardrail,
};
use agentos_interfaces::orchestrator::RunContext;
use agentos_proto::{Message, ToolCall, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct PiiFilter;

#[async_trait]
impl InputGuardrail for PiiFilter {
    async fn check(
        &self,
        input: &Input,
        _ctx: &RunContext<'_>,
    ) -> Result<GuardrailOutcome, GuardrailError> {
        if contains_email_like(&input.message.content) {
            return Ok(GuardrailOutcome::Tripped(Arc::from(
                "input appears to contain an email address",
            )));
        }
        if contains_ssn_like(&input.message.content) {
            return Ok(GuardrailOutcome::Tripped(Arc::from(
                "input appears to contain a US social security number",
            )));
        }
        Ok(GuardrailOutcome::Passed)
    }
}

pub struct MaxOutputLength {
    max_chars: usize,
}

impl MaxOutputLength {
    pub fn new(max_chars: usize) -> Self {
        Self { max_chars }
    }
}

#[async_trait]
impl OutputGuardrail for MaxOutputLength {
    async fn check(
        &self,
        output: &Message,
        _ctx: &RunContext<'_>,
    ) -> Result<GuardrailOutcome, GuardrailError> {
        let chars = output.content.chars().count();
        if chars > self.max_chars {
            return Ok(GuardrailOutcome::Tripped(Arc::from(format!(
                "output has {chars} characters, limit is {}",
                self.max_chars
            ))));
        }
        Ok(GuardrailOutcome::Passed)
    }
}

pub struct ShellCommandAllowlist {
    allowed: BTreeSet<Arc<str>>,
}

impl ShellCommandAllowlist {
    pub fn new(commands: impl IntoIterator<Item = impl Into<Arc<str>>>) -> Self {
        Self {
            allowed: commands.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ShellCallArgs {
    command: String,
}

#[async_trait]
impl ToolGuardrail for ShellCommandAllowlist {
    async fn check_call(
        &self,
        call: &ToolCall,
        _ctx: &RunContext<'_>,
    ) -> Result<GuardrailOutcome, GuardrailError> {
        if call.name.as_ref() != "shell" {
            return Ok(GuardrailOutcome::Passed);
        }

        let parsed: ShellCallArgs = serde_json::from_str(call.args.get())
            .map_err(|err| GuardrailError::Backend(err.to_string().into()))?;
        if parsed.command.split_whitespace().nth(1).is_some() {
            return Ok(GuardrailOutcome::Tripped(Arc::from(format!(
                "shell command '{}' includes arguments in the command field; use command='<program>' and put arguments in the structured args array",
                parsed.command
            ))));
        }
        if self.allowed.contains(parsed.command.as_str()) {
            Ok(GuardrailOutcome::Passed)
        } else {
            Ok(GuardrailOutcome::Tripped(Arc::from(format!(
                "shell command '{}' is not allowlisted; allowed commands: {}",
                parsed.command,
                self.allowed
                    .iter()
                    .map(|command| command.as_ref())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))))
        }
    }

    async fn check_result(
        &self,
        _result: &ToolResult,
        _ctx: &RunContext<'_>,
    ) -> Result<GuardrailOutcome, GuardrailError> {
        Ok(GuardrailOutcome::Passed)
    }
}

/// Hard boundary: refuses `file` writes that resolve inside the skill-bundle
/// directory. Wired into every sub-agent that is not the designated skill
/// editor, so a sub-agent with blanket `file` write (e.g. the code-fix agent)
/// cannot tamper with `SKILL.md` bundles even if steered to. Reads and
/// directory listings under the skills tree are unaffected.
pub struct SkillBundleWriteGuardrail;

#[derive(Debug, Deserialize)]
struct FileWriteArgs {
    operation: String,
    path: PathBuf,
}

/// Pure decision core: returns the resolved on-disk target when `call` is a
/// `file` *write* that lands inside `skills_root`, else `None`. Resolution
/// reuses `safe_workspace_path`, so `..`, absolute, and root-prefixed paths
/// are rejected by the same logic the file tool enforces — a `Some` result
/// cannot be produced by a traversal trick that escapes the skills tree.
fn skill_bundle_write_target(
    call: &ToolCall,
    ws_root: &Path,
    skills_root: &Path,
) -> Option<PathBuf> {
    if call.name.as_ref() != "file" {
        return None;
    }
    let parsed: FileWriteArgs = serde_json::from_str(call.args.get()).ok()?;
    if parsed.operation != "write" {
        return None;
    }
    // A path the file tool would itself reject (absolute / `..` / empty) is
    // not a skill-bundle write — let the tool surface its own error.
    let resolved = safe_workspace_path(ws_root, &parsed.path).ok()?;
    resolved.starts_with(skills_root).then_some(resolved)
}

#[async_trait]
impl ToolGuardrail for SkillBundleWriteGuardrail {
    async fn check_call(
        &self,
        call: &ToolCall,
        _ctx: &RunContext<'_>,
    ) -> Result<GuardrailOutcome, GuardrailError> {
        match skill_bundle_write_target(call, &workspace_root(), &skills_dir()) {
            Some(target) => Ok(GuardrailOutcome::Tripped(Arc::from(format!(
                "write to '{}' is inside the skill-bundle directory; skill-bundle edits must go through the skill-editor sub-agent via the skill_ops route, not this sub-agent",
                target.display()
            )))),
            None => Ok(GuardrailOutcome::Passed),
        }
    }

    async fn check_result(
        &self,
        _result: &ToolResult,
        _ctx: &RunContext<'_>,
    ) -> Result<GuardrailOutcome, GuardrailError> {
        Ok(GuardrailOutcome::Passed)
    }
}

fn contains_email_like(input: &str) -> bool {
    input.split_whitespace().any(|token| {
        let Some((local, domain)) = token.split_once('@') else {
            return false;
        };
        !local.is_empty() && domain.contains('.') && !domain.starts_with('.')
    })
}

fn contains_ssn_like(input: &str) -> bool {
    input.as_bytes().windows(11).any(|window| {
        window[0].is_ascii_digit()
            && window[1].is_ascii_digit()
            && window[2].is_ascii_digit()
            && window[3] == b'-'
            && window[4].is_ascii_digit()
            && window[5].is_ascii_digit()
            && window[6] == b'-'
            && window[7].is_ascii_digit()
            && window[8].is_ascii_digit()
            && window[9].is_ascii_digit()
            && window[10].is_ascii_digit()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_proto::{ToolCall, ToolCallId};
    use serde_json::{json, value::RawValue};

    fn shell_call(command: &str) -> ToolCall {
        ToolCall {
            id: ToolCallId::new("shell-test"),
            name: Arc::from("shell"),
            args: RawValue::from_string(json!({ "command": command }).to_string()).unwrap(),
        }
    }

    fn file_call(operation: &str, path: &str) -> ToolCall {
        ToolCall {
            id: ToolCallId::new("file-test"),
            name: Arc::from("file"),
            args: RawValue::from_string(
                json!({ "operation": operation, "path": path, "content": "x" }).to_string(),
            )
            .unwrap(),
        }
    }

    fn roots() -> (PathBuf, PathBuf) {
        let ws = PathBuf::from("/ws");
        let skills = ws.join("workspace").join("skills");
        (ws, skills)
    }

    #[test]
    fn trips_on_skill_bundle_write() {
        let (ws, skills) = roots();
        let target = skill_bundle_write_target(
            &file_call("write", "workspace/skills/audit-skill/SKILL.md"),
            &ws,
            &skills,
        );
        assert_eq!(
            target,
            Some(ws.join("workspace/skills/audit-skill/SKILL.md"))
        );
    }

    #[test]
    fn allows_skill_bundle_read_and_listing() {
        let (ws, skills) = roots();
        assert_eq!(
            skill_bundle_write_target(
                &file_call("read", "workspace/skills/audit-skill/SKILL.md"),
                &ws,
                &skills
            ),
            None
        );
    }

    #[test]
    fn allows_write_outside_skill_bundles() {
        let (ws, skills) = roots();
        assert_eq!(
            skill_bundle_write_target(
                &file_call("write", "crates/agentos-core/src/lib.rs"),
                &ws,
                &skills
            ),
            None
        );
    }

    #[test]
    fn traversal_into_skills_is_rejected_not_allowed() {
        let (ws, skills) = roots();
        // `safe_workspace_path` rejects any `..`, so this never resolves to a
        // skills path that slips past the guard.
        assert_eq!(
            skill_bundle_write_target(
                &file_call("write", "crates/../workspace/skills/x/SKILL.md"),
                &ws,
                &skills
            ),
            None
        );
    }

    #[test]
    fn non_file_tool_is_ignored() {
        let (ws, skills) = roots();
        assert_eq!(
            skill_bundle_write_target(&shell_call("ls"), &ws, &skills),
            None
        );
    }

    #[tokio::test]
    async fn shell_allowlist_rejects_arguments_in_command_field_with_hint() {
        let guardrail = ShellCommandAllowlist::new(["find"]);
        let ctx_state = agentos_interfaces::RunState::new(
            agentos_proto::RunId::new("run"),
            agentos_proto::AgentId::new("agent"),
        );
        let ctx = RunContext::from_state(&ctx_state);

        let outcome = guardrail
            .check_call(&shell_call("find workspace"), &ctx)
            .await
            .expect("guardrail should run");

        match outcome {
            GuardrailOutcome::Tripped(reason) => {
                assert!(reason.contains("structured args array"));
            }
            GuardrailOutcome::Passed => panic!("command with embedded args should trip"),
        }
    }
}
