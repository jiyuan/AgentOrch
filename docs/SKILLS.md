# Workspace Skills

AgentOS stores user-defined skills in `workspace/skills/`. Each skill is a folder named after the skill, with a required `SKILL.md` file using the Anthropic skills format.

## Layout

```text
workspace/skills/
└── skill-name/
    ├── SKILL.md
    ├── scripts/
    ├── references/
    └── assets/
```

Only `SKILL.md` is required. Add bundled resource directories only when the skill needs deterministic scripts, detailed references, or reusable output assets.

## Standard Workflow

1. Create: `agentos skill create <name> <description>`.
2. Edit: keep `SKILL.md` concise and move detailed material into bundled resources.
3. Validate: `agentos skill validate <name>` or `agentos skill validate --all`.
4. Enable: add the skill name under `[resources.skills].enabled` in `workspace/agent.toml`.
5. Execute: runtime loading validates enabled skills, adds their descriptions to the resource index, and only lets deterministic skill planners emit normal AgentOS plans after validation succeeds.

## Anthropic Compatibility

Validation checks that migrated and newly created skills follow the `SKILL.md` format:

- Frontmatter is delimited by `---`.
- `name` and `description` are present.
- `name` is lowercase hyphen-case and matches the folder name.
- Markdown body instructions are non-empty.
- Optional frontmatter is limited to `license`, `allowed-tools`, and `metadata`.

Legacy deterministic skills should be migrated by creating a matching folder in `workspace/skills/`, validating it, and changing any config names from underscore style to hyphen-case.
