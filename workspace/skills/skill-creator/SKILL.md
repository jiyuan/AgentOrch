---
name: skill-creator
description: Create, update, and validate workspace skills using the Anthropic SKILL.md folder format. Use when adding a reusable AgentOS workflow, migrating a legacy skill, or checking skill compatibility.
---

# Skill Creator

Create every skill as a self-contained folder in `workspace/skills/<skill-name>/` with a required `SKILL.md`. Follow the Anthropic skills pattern: YAML frontmatter for metadata, Markdown body instructions, and optional bundled resource directories.

## Creation Workflow

1. Normalize the skill name to lowercase hyphen-case.
2. Create `workspace/skills/<skill-name>/SKILL.md`.
3. Write YAML frontmatter with required `name` and `description` fields.
4. Put repeatable execution guidance in the Markdown body.
5. Add optional `scripts/`, `references/`, or `assets/` directories only when they directly support the workflow.
6. Run `agentos skill validate <skill-name>` before enabling or migrating the skill.

## Execution Workflow

AgentOS loads workspace skills at runtime, validates their `SKILL.md` files, indexes their descriptions as skill resources, and only activates deterministic skill planners after validation succeeds. Skill planners must return normal AgentOS plans, such as tool calls or replies, so approval and guardrails remain in the run loop.

## CLI Utility

Use the built-in CLI commands as the local skill-creator utility:

```sh
agentos skill create <name> <description> --resources=scripts,references,assets
agentos skill validate <name>
agentos skill validate --all
agentos skill list
```

## Validation

Validation checks the Anthropic-compatible shape: `SKILL.md` exists, frontmatter starts and ends with `---`, `name` and `description` are present, the name is lowercase hyphen-case, the folder name matches the skill name, and Markdown body instructions are non-empty.
