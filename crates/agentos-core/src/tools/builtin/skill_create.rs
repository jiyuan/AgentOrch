use super::common::{default_skills_dir, elapsed_ms, result_metadata, skills_root_for_tests};
use crate::skills::{create_skill, SkillBundleFile, SkillCreation, SkillResourceKind};
use agentos_interfaces::tool::{Tool, ToolError, ToolSpec};
use agentos_proto::{ToolCall, ToolResult, ToolStatus};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, value::RawValue};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

/// Tool wrapping `crate::skills::create_skill` so model-driven sub-agents can
/// scaffold new workspace skills end-to-end (folder, frontmatter SKILL.md,
/// optional resource subdirectories, and validation pass) inside the normal
/// run-loop approval and guardrail flow.
#[derive(Default)]
pub struct SkillCreatorTool;

/// Deserialised tool input. `root` is intentionally *not* exposed on the
/// LLM-visible schema and not accepted from callers — the runtime resolves
/// the skills directory itself (matching the cron-tool fix). Tests redirect
/// writes via `TEST_SKILLS_DIR` in `super::common`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillCreateArgs {
    name: String,
    description: String,
    #[serde(default)]
    resources: Vec<String>,
    /// Optional Markdown body for SKILL.md (without the YAML frontmatter
    /// — the tool rebuilds the frontmatter from `name` + `description`).
    /// Use this to produce a complete SKILL.md in one tool call rather
    /// than scaffolding and then editing.
    #[serde(default)]
    body: Option<String>,
    /// Optional bundle files to write under the skill directory. Each
    /// path is relative to the skill directory, may not contain `..`,
    /// and may not be the literal `SKILL.md` (use `body` for that).
    /// Lets the LLM produce scripts/, references/, and assets/ contents
    /// in the same call.
    #[serde(default)]
    files: Vec<SkillCreateFileArgs>,
    /// When `true` and the skill already exists, replace the existing
    /// bundle. Without this, a retry after a failed/incomplete first
    /// call returns an `already exists` error. Set this when recovering
    /// from a partial scaffold or when the user explicitly asks to
    /// overwrite.
    #[serde(default)]
    replace: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillCreateFileArgs {
    path: String,
    content: String,
}

#[async_trait]
impl Tool for SkillCreatorTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: Arc::from("skill_create"),
            description: Arc::from(
                "Create a complete workspace skill bundle in one call. \
                 Required behavior: when the user asks for a real (non-trivial) \
                 skill, always provide `body` with the full SKILL.md Markdown \
                 instructions AND `files` with every script/reference/asset \
                 the skill needs to function. Do NOT call this tool with only \
                 `name` and `description` unless the user explicitly asked \
                 for an empty scaffold — that produces a useless directory \
                 the user will have to fill in by hand. \
                 The tool writes workspace/skills/<name>/SKILL.md (rebuilding \
                 the YAML frontmatter from `name` + `description`), creates \
                 the requested resource subdirectories, writes every `files[]` \
                 entry under the skill directory, and validates the result \
                 before returning. If a previous call left an incomplete \
                 skeleton, set `replace: true` to overwrite it rather than \
                 retrying with the same args.",
            ),
            input_schema: json!({
                "type": "object",
                "required": ["name", "description"],
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Skill name in lowercase hyphen-case (e.g. rss-digest)."
                    },
                    "description": {
                        "type": "string",
                        "description": "One-line description used as the catalog summary."
                    },
                    "resources": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["scripts", "references", "assets"] },
                        "description": "Optional bundled resource subdirectories to create."
                    },
                    "body": {
                        "type": "string",
                        "description": "Optional Markdown body for SKILL.md (no frontmatter — the tool rebuilds it). When omitted, a placeholder scaffold is written and the agent is expected to follow up with edits."
                    },
                    "files": {
                        "type": "array",
                        "description": "Optional bundle files to write inside the skill directory in the same call. Each item has a relative `path` (no '..'; not the literal 'SKILL.md') and `content` string.",
                        "items": {
                            "type": "object",
                            "required": ["path", "content"],
                            "properties": {
                                "path": { "type": "string" },
                                "content": { "type": "string" }
                            }
                        }
                    },
                    "replace": {
                        "type": "boolean",
                        "description": "When true and the skill already exists, replace the existing bundle. Use this to recover from a partial earlier call (e.g. a previous skill_create produced an empty skeleton)."
                    }
                }
            }),
            requires_isolation: false,
        }
    }

    async fn call(&self, call: &ToolCall, args: &RawValue) -> Result<ToolResult, ToolError> {
        let parsed: SkillCreateArgs = serde_json::from_str(args.get())
            .map_err(|err| ToolError::Failed(err.to_string().into()))?;
        let start = Instant::now();

        let mut resources = BTreeSet::new();
        for entry in &parsed.resources {
            let kind = match entry.as_str() {
                "scripts" => SkillResourceKind::Scripts,
                "references" => SkillResourceKind::References,
                "assets" => SkillResourceKind::Assets,
                other => {
                    return Err(ToolError::Failed(Arc::from(format!(
                        "unknown resource kind '{other}'; expected scripts, references, or assets"
                    ))));
                }
            };
            resources.insert(kind);
        }

        let files = parsed
            .files
            .into_iter()
            .map(|file| SkillBundleFile {
                path: PathBuf::from(file.path),
                content: Arc::from(file.content),
            })
            .collect::<Vec<_>>();
        let file_count = files.len();

        let creation = SkillCreation {
            name: Arc::from(parsed.name.as_str()),
            description: Arc::from(parsed.description.as_str()),
            resources,
            body: parsed.body.as_deref().map(Arc::from),
            files,
            replace: parsed.replace,
        };
        let root = skills_root_for_tests().unwrap_or_else(default_skills_dir);
        let skill = create_skill(&root, &creation)
            .map_err(|err| ToolError::Failed(Arc::from(err.to_string())))?;

        // Audit what's actually on disk under the canonical skill path and
        // report it in the success message. If a write silently failed the
        // count won't match the message, so the user (and the LLM on the
        // next turn) can detect the discrepancy. Counts include SKILL.md
        // and every file under the skill directory recursively.
        let audit = audit_skill_dir(&skill.path);
        let message = format!(
            "created skill '{}' at {} ({} file{} on disk; requested {} bundle file{})",
            skill.name,
            skill.path.display(),
            audit.total_files,
            if audit.total_files == 1 { "" } else { "s" },
            file_count,
            if file_count == 1 { "" } else { "s" },
        );
        let bytes_out = message.len() as u64;
        Ok(ToolResult {
            call_id: call.id.clone(),
            status: ToolStatus::Succeeded,
            content: Arc::from(message),
            metadata: result_metadata(elapsed_ms(start), bytes_out),
        })
    }
}

struct SkillDirAudit {
    total_files: usize,
}

fn audit_skill_dir(path: &std::path::Path) -> SkillDirAudit {
    let mut total_files = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let entry_path = entry.path();
            if entry_path.is_dir() {
                stack.push(entry_path);
            } else {
                total_files += 1;
            }
        }
    }
    SkillDirAudit { total_files }
}

#[cfg(test)]
mod tests {
    use super::super::common::test_support::{tool_call, SkillsDirGuard};
    use super::*;

    #[tokio::test]
    async fn skill_creator_tool_scaffolds_skill_dir_and_markdown() {
        let guard = SkillsDirGuard::new("skill-creator-tool");
        let args = json!({
            "name": "test-skill",
            "description": "Smoke test for SkillCreatorTool.",
            "resources": ["scripts"],
        });
        let raw = RawValue::from_string(args.to_string()).unwrap();
        let result = SkillCreatorTool
            .call(&tool_call("skill_create", "call_1"), &raw)
            .await
            .unwrap();
        assert_eq!(result.status, ToolStatus::Succeeded);
        assert!(result.content.contains("test-skill"));
        let skill_md = guard.dir.join("test-skill").join("SKILL.md");
        let body = std::fs::read_to_string(&skill_md).unwrap();
        assert!(body.contains("name: test-skill"));
        assert!(body.contains("Smoke test for SkillCreatorTool."));
        assert!(guard.dir.join("test-skill").join("scripts").is_dir());
    }

    #[tokio::test]
    async fn skill_creator_tool_rejects_root_override_from_caller() {
        // Regression: the model could previously pass `root="workspace"`
        // and have SKILL.md land outside `workspace/skills/`. Schema no
        // longer advertises `root`, and deny_unknown_fields makes silent
        // misuse impossible.
        let _guard = SkillsDirGuard::new("skill-creator-rooted");
        let args = json!({
            "name": "rooted-skill",
            "description": "Attempts directory escape.",
            "root": "workspace",
        });
        let raw = RawValue::from_string(args.to_string()).unwrap();
        let err = SkillCreatorTool
            .call(&tool_call("skill_create", "call_root"), &raw)
            .await
            .unwrap_err();
        let ToolError::Failed(msg) = err;
        assert!(msg.contains("unknown field") && msg.contains("root"));
    }

    #[tokio::test]
    async fn skill_creator_tool_rejects_unknown_resource() {
        let _guard = SkillsDirGuard::new("skill-creator-bad");
        let args = json!({
            "name": "another-skill",
            "description": "n/a",
            "resources": ["nope"],
        });
        let raw = RawValue::from_string(args.to_string()).unwrap();
        let err = SkillCreatorTool
            .call(&tool_call("skill_create", "call_2"), &raw)
            .await
            .unwrap_err();
        let ToolError::Failed(msg) = err;
        assert!(msg.contains("unknown resource kind"));
    }

    #[tokio::test]
    async fn skill_creator_tool_writes_body_and_bundle_files_atomically() {
        // One tool call produces a complete bundle — SKILL.md with the
        // caller-supplied body, plus three bundle files at the requested
        // paths. This is the path the upgraded skill-creator workflow
        // depends on: no follow-up file writes needed.
        let guard = SkillsDirGuard::new("skill-creator-bundle");
        let args = json!({
            "name": "audit-skill",
            "description": "Pushy description for triggering.",
            "resources": ["scripts", "references"],
            "body": "# Audit Skill\n\nThis is the body.\n",
            "files": [
                { "path": "scripts/run.py", "content": "print('hi')\n" },
                { "path": "references/notes.md", "content": "Notes.\n" },
                { "path": "scripts/lib/util.py", "content": "x = 1\n" }
            ],
        });
        let raw = RawValue::from_string(args.to_string()).unwrap();
        let result = SkillCreatorTool
            .call(&tool_call("skill_create", "bundle"), &raw)
            .await
            .unwrap();
        assert_eq!(result.status, ToolStatus::Succeeded);
        assert!(result.content.contains("3 bundle files"));

        let skill_dir = guard.dir.join("audit-skill");
        let skill_md = std::fs::read_to_string(skill_dir.join("SKILL.md")).unwrap();
        assert!(skill_md.contains("name: audit-skill"));
        assert!(skill_md.contains("Pushy description for triggering."));
        assert!(skill_md.contains("# Audit Skill"));
        assert!(skill_md.contains("This is the body."));
        // Default scaffold text must NOT leak in when body is supplied.
        assert!(!skill_md.contains("Describe the repeatable workflow"));

        let run_py = std::fs::read_to_string(skill_dir.join("scripts").join("run.py")).unwrap();
        assert_eq!(run_py, "print('hi')\n");
        let notes_md =
            std::fs::read_to_string(skill_dir.join("references").join("notes.md")).unwrap();
        assert_eq!(notes_md, "Notes.\n");
        // Nested path creates intermediate dirs.
        let util_py =
            std::fs::read_to_string(skill_dir.join("scripts").join("lib").join("util.py")).unwrap();
        assert_eq!(util_py, "x = 1\n");
    }

    #[tokio::test]
    async fn skill_creator_tool_rejects_parent_dir_traversal_in_bundle_path() {
        let guard = SkillsDirGuard::new("skill-creator-traverse");
        let args = json!({
            "name": "escape-skill",
            "description": "Tries to climb out.",
            "files": [
                { "path": "../escaped.txt", "content": "nope" }
            ],
        });
        let raw = RawValue::from_string(args.to_string()).unwrap();
        let err = SkillCreatorTool
            .call(&tool_call("skill_create", "traverse"), &raw)
            .await
            .unwrap_err();
        let ToolError::Failed(msg) = err;
        assert!(
            msg.contains("'..'"),
            "expected traversal rejection, got: {msg}"
        );
        // The skill_dir must not exist — failure happens mid-write and the
        // bundle write step is reached only after SKILL.md, so the skill
        // dir may be partially populated. The important guarantee is that
        // nothing landed outside the guard's tmpdir.
        let escaped = guard.dir.parent().unwrap().join("escaped.txt");
        assert!(!escaped.exists());
    }

    #[tokio::test]
    async fn skill_creator_tool_rejects_absolute_bundle_path() {
        let _guard = SkillsDirGuard::new("skill-creator-absolute");
        let args = json!({
            "name": "absolute-skill",
            "description": "Tries an absolute write.",
            "files": [
                { "path": "/tmp/agentos-pwn.txt", "content": "nope" }
            ],
        });
        let raw = RawValue::from_string(args.to_string()).unwrap();
        let err = SkillCreatorTool
            .call(&tool_call("skill_create", "absolute"), &raw)
            .await
            .unwrap_err();
        let ToolError::Failed(msg) = err;
        assert!(
            msg.contains("relative"),
            "expected absolute-path rejection, got: {msg}"
        );
        assert!(!std::path::Path::new("/tmp/agentos-pwn.txt").exists());
    }

    #[tokio::test]
    async fn skill_creator_tool_replaces_existing_bundle_when_replace_set() {
        // Recovery path for the user's reported flow: first call left a
        // skeleton, second call with replace=true overwrites it with the
        // full bundle.
        let guard = SkillsDirGuard::new("skill-creator-replace");

        // Initial empty scaffold.
        let scaffold_args = json!({
            "name": "redo-skill",
            "description": "Initial empty scaffold from a botched first try.",
        });
        let raw = RawValue::from_string(scaffold_args.to_string()).unwrap();
        SkillCreatorTool
            .call(&tool_call("skill_create", "first"), &raw)
            .await
            .unwrap();

        // Retry without replace must fail with the recovery hint.
        let retry_args = json!({
            "name": "redo-skill",
            "description": "Retry attempt that should be rejected.",
        });
        let raw = RawValue::from_string(retry_args.to_string()).unwrap();
        let err = SkillCreatorTool
            .call(&tool_call("skill_create", "retry"), &raw)
            .await
            .unwrap_err();
        let ToolError::Failed(msg) = err;
        assert!(
            msg.contains("already exists") && msg.contains("replace"),
            "expected `already exists` + recovery hint, got: {msg}"
        );

        // Retry with replace=true succeeds and overwrites.
        let replace_args = json!({
            "name": "redo-skill",
            "description": "Full bundle on the recovery pass.",
            "body": "# Redo Skill\n\nThis is the recovery body.\n",
            "files": [
                { "path": "scripts/run.py", "content": "print('recovered')\n" }
            ],
            "replace": true,
        });
        let raw = RawValue::from_string(replace_args.to_string()).unwrap();
        let result = SkillCreatorTool
            .call(&tool_call("skill_create", "replace"), &raw)
            .await
            .unwrap();
        assert_eq!(result.status, ToolStatus::Succeeded);
        let skill_md =
            std::fs::read_to_string(guard.dir.join("redo-skill").join("SKILL.md")).unwrap();
        assert!(skill_md.contains("recovery body"));
        assert!(!skill_md.contains("Initial empty scaffold"));
        let run_py =
            std::fs::read_to_string(guard.dir.join("redo-skill").join("scripts").join("run.py"))
                .unwrap();
        assert_eq!(run_py, "print('recovered')\n");
    }

    #[tokio::test]
    async fn skill_creator_tool_message_disk_count_matches_actual_files() {
        // Audit guard: the tool message reports both the requested file
        // count and the actual on-disk file count. These must agree on a
        // healthy filesystem. If they don't, the user can see the
        // discrepancy in the reply instead of being told a success that
        // didn't happen.
        let guard = SkillsDirGuard::new("skill-creator-audit");
        let args = json!({
            "name": "audit-skill",
            "description": "Audit-test that disk count matches message.",
            "resources": ["scripts", "references"],
            "body": "# Audit Skill\n\nBody body body.\n",
            "files": [
                { "path": "scripts/aggregate.py", "content": "x = 1\n" },
                { "path": "references/notes.md", "content": "notes\n" }
            ],
        });
        let raw = RawValue::from_string(args.to_string()).unwrap();
        let result = SkillCreatorTool
            .call(&tool_call("skill_create", "audit"), &raw)
            .await
            .unwrap();
        assert_eq!(result.status, ToolStatus::Succeeded);
        // SKILL.md + 2 bundle files = 3 files on disk total.
        assert!(
            result.content.contains("3 files on disk"),
            "expected message to report 3 files on disk, got: {}",
            result.content
        );
        assert!(
            result.content.contains("requested 2 bundle files"),
            "expected message to report 2 requested bundle files, got: {}",
            result.content
        );

        // Independently verify on disk.
        let skill_dir = guard.dir.join("audit-skill");
        assert!(skill_dir.join("SKILL.md").is_file());
        assert!(skill_dir.join("scripts").join("aggregate.py").is_file());
        assert!(skill_dir.join("references").join("notes.md").is_file());
    }

    #[tokio::test]
    async fn skill_creator_tool_reports_absolute_path_in_message() {
        // The skill_create tool's success message must point at an
        // absolute, canonicalised path so users with a different shell
        // CWD than the gateway process can still find the bundle.
        let guard = SkillsDirGuard::new("skill-creator-abspath");
        let args = json!({
            "name": "abs-skill",
            "description": "Path-in-message audit.",
            "body": "# Abs\n\nbody.\n",
        });
        let raw = RawValue::from_string(args.to_string()).unwrap();
        let result = SkillCreatorTool
            .call(&tool_call("skill_create", "abs"), &raw)
            .await
            .unwrap();
        // SkillsDirGuard creates a tempdir at an absolute path. The message
        // must include that absolute path, not a relative one.
        let canonical = guard.dir.canonicalize().unwrap();
        let expected = canonical.join("abs-skill");
        assert!(
            result.content.contains(&*expected.to_string_lossy()),
            "expected message to contain absolute path {}, got: {}",
            expected.display(),
            result.content
        );
    }

    #[tokio::test]
    async fn skill_creator_tool_rejects_skill_md_in_files() {
        let _guard = SkillsDirGuard::new("skill-creator-skillmd");
        let args = json!({
            "name": "shadow-skill",
            "description": "Tries to shadow SKILL.md.",
            "files": [
                { "path": "SKILL.md", "content": "---\nname: shadow\n---\n" }
            ],
        });
        let raw = RawValue::from_string(args.to_string()).unwrap();
        let err = SkillCreatorTool
            .call(&tool_call("skill_create", "shadow"), &raw)
            .await
            .unwrap_err();
        let ToolError::Failed(msg) = err;
        assert!(
            msg.contains("body"),
            "expected pointer to body field, got: {msg}"
        );
    }
}
