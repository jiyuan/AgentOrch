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

1. Create: `agentos skill create <name> <description> [--resources=scripts,references,assets]`.
   The optional `--resources` flag scaffolds the matching bundled directories.
2. Edit: keep `SKILL.md` concise and move detailed material into bundled resources.
3. Validate: `agentos skill validate <name>` or `agentos skill validate --all`.
4. List: `agentos skill list` prints each skill name and description.
5. Enable: add the skill name under `[resources.skills].enabled` in `workspace/agent.toml`.
6. Execute: runtime loading validates enabled skills, adds their descriptions to the resource index, and only lets deterministic skill planners emit normal AgentOS plans after validation succeeds.

## Anthropic Compatibility

Validation checks that migrated and newly created skills follow the `SKILL.md` format:

- Frontmatter is delimited by `---`.
- `name` and `description` are present and scalar values.
- `name` is lowercase hyphen-case, matches the folder name, and does not start
  or end with a hyphen or contain consecutive hyphens.
- `description` is at most 1024 characters and contains no angle brackets.
- Markdown body instructions are non-empty.
- Optional frontmatter is limited to `license`, `allowed-tools`, and `metadata`;
  any other key is rejected.

Legacy deterministic skills should be migrated by creating a matching folder in `workspace/skills/`, validating it, and changing any config names from underscore style to hyphen-case.
