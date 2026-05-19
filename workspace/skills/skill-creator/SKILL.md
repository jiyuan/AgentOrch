---
name: skill-creator
description: Create, update, and validate AgentOS workspace skills. Use this skill whenever the user asks to create a skill, improve an existing skill, migrate a workflow into SKILL.md format, tune skill triggering, or fix a skill validation failure.
---

# Skill Creator

Use this skill to build AgentOS skills as durable, validated workflow bundles. The LLM is the generator: it interviews, drafts, rewrites, and improves the skill content. AgentOS is the validator: the `skill_validate` tool and `agentos skill validate` CLI command are the source of truth for whether the bundle is structurally acceptable.

Do not report that a skill is complete until validation passes.

## Runtime Contract

1. Capture the user's intent, including the workflow, trigger conditions, inputs, outputs, required tools, edge cases, and success criteria.
2. Draft the intended bundle shape before writing files: decide whether the skill needs only `SKILL.md` or also `scripts/`, `references/`, or `assets/`.
3. Write all supporting resource files first with the `file` tool. Write `SKILL.md` last so validation happens after the planned bundle exists.
4. Generate a complete `workspace/skills/<skill-name>/SKILL.md` with valid frontmatter and useful Markdown instructions that point to any support files you created.
5. Run `skill_validate` for the skill name.
6. If validation fails, read the failure reason, revise the generated files, write the fix, and run `skill_validate` again.
7. If validation passes, inspect the validator's `bundle_inventory`. PASS confirms `SKILL.md` structure only; it does not prove the bundle is complete. If the inventory is missing files required by the user's requested workflow or by `SKILL.md`, write the missing files and run `skill_validate` again.
8. Repeat the generate -> validate -> inventory-check -> repair loop until validation passes and the bundle inventory matches the intended bundle.
9. After the bundle is complete, summarize the skill path, every file created, and any recommended test prompts or follow-up improvements.

## Skill Shape

Every AgentOS skill is a directory with:

```text
workspace/skills/<skill-name>/
  SKILL.md
  scripts/       optional deterministic helpers
  references/    optional detailed docs loaded only when needed
  assets/        optional templates or static assets
```

`SKILL.md` must start with YAML frontmatter:

```markdown
---
name: lower-hyphen-name
description: Clear trigger description explaining when to use the skill.
---

# Skill Title

Markdown instructions for the reusable workflow.
```

The `name` must match the folder name. The description is the trigger surface; make it specific enough that the model knows when to use the skill, including common phrases and adjacent contexts where the skill should apply.

## Generation Guidance

Write skills for repeated use, not just the immediate example. Prefer concise instructions with clear reasoning over rigid command lists. Keep the body focused on the core workflow; move bulky schemas, long examples, or domain references into `references/` and point to them from `SKILL.md`.

Include these sections when they materially help:

- `## When To Use`: concrete trigger situations and near-misses.
- `## Inputs`: arguments, files, URLs, or context the user must provide.
- `## Workflow`: ordered steps with success criteria.
- `## Tool Use`: required AgentOS tools or CLI commands.
- `## Validation`: how to check the skill after edits.
- `## Examples`: realistic prompts that should trigger the skill.

For deterministic or repetitive work, add a script under `scripts/` and instruct the LLM when to run it. For reference-heavy domains, add targeted files under `references/` and explain when to open each one. For templates, icons, or sample files, use `assets/`.

Use these bundle rules:

- Simple judgment or writing workflows may be a single-file bundle with only `SKILL.md`.
- Deterministic parsing, calculations, audits, report generation, migrations, or checks should include at least one helper under `scripts/`.
- Workflows with schemas, field mappings, long examples, rubrics, or domain facts should include targeted files under `references/`.
- Workflows that reuse templates, samples, images, icons, or fixture files should include them under `assets/`.
- Do not claim that a bundle is complete if `bundle_inventory` lists only `SKILL.md` and the workflow would be more reliable with a script, reference, or asset.

## Validation Loop

Validation failures are expected feedback, not fatal errors. Treat the validator output as a repair instruction.

If `skill_validate` reports:

- missing frontmatter: rewrite `SKILL.md` with opening and closing `---` delimiters.
- missing name or description: add valid scalar frontmatter fields.
- folder/name mismatch: make the frontmatter `name` exactly match the directory.
- empty body: add real workflow instructions after frontmatter.
- malformed YAML or unsupported frontmatter: simplify to supported keys and retry.

Only stop after a `PASS` result, a user cancellation, or repeated failures where you need clarification.

After a `PASS`, read the `bundle_inventory` in the validator result. If `SKILL.md` references a support path that is absent from the inventory, write that missing file and validate again. If the user asked for a complete bundle and the inventory contains only `SKILL.md`, either create the support files that make the workflow durable or explicitly explain why this skill is intentionally single-file.

## Evaluation and Improvement

After a new skill validates, propose 2-3 realistic test prompts. For objective workflows, suggest checks that can prove success. For subjective workflows, ask the user to review outputs and refine the instructions based on feedback.

When improving an existing skill:

1. Preserve the existing skill name and folder.
2. Read the current `SKILL.md`.
3. Identify whether the issue is triggering, workflow quality, missing resources, or validation.
4. Revise the smallest useful part.
5. Validate after each batch of changes.

Description tuning is part of skill quality. If a skill fails to trigger when it should, rewrite the description to include stronger trigger contexts and realistic user phrasings.

## Security and Scope

Do not create skills that hide behavior, exfiltrate data, bypass approvals, or instruct the agent to ignore user or system policy. A skill should not surprise the user relative to its name and description.

Use the minimum necessary tools and resources. Keep generated skills inspectable, portable, and easy to validate.
