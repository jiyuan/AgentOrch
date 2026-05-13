# AgentOS deltas

This skill is adapted from the upstream Anthropic [skill-creator](https://github.com/anthropics/skills/tree/main/skills/skill-creator).
AgentOS is a different runtime — most of the workflow guidance translates
verbatim, but a handful of tools and conventions differ. Read this when the
main `SKILL.md` (or upstream docs) reference something that doesn't exist
here.

## What's the same

- The SKILL.md folder format: YAML frontmatter + Markdown body, optional
  `scripts/`, `references/`, `assets/` directories.
- Frontmatter validation rules: lowercase hyphen-case `name` ≤ 64 chars,
  `description` ≤ 1024 chars with no angle brackets, folder name matches
  `name`.
- The packaging shape: `<skill-name>.skill` is a zip with the skill folder
  at the archive root.
- The skill writing philosophy: progressive disclosure, "pushy"
  descriptions, lean prompts, explain the *why*, bundle repeated work into
  `scripts/`.
- The iteration loop: draft → run on test cases → review with user →
  improve → repeat until satisfied → package.

## What's different

### No upstream Python tooling

The upstream `scripts/aggregate_benchmark.py`, `eval-viewer/generate_review.py`,
`scripts/run_loop.py`, `scripts/run_eval.py`, and `scripts/improve_description.py`
target the Claude Code / Cowork environment. They depend on the `claude` CLI
subprocess, a local browser/display, and Anthropic's eval pipeline conventions.

AgentOS does **not** ship them. The portable pieces — `quick_validate.py` and
`package_skill.py` — are included under `scripts/`.

When you need to:

- **Review test outputs with the user** → present them inline in the
  conversation. Save file outputs to disk and tell the user the path.
- **Benchmark with-skill vs without-skill** → run each test case twice
  (once with the skill enabled in the sub-agent config, once without) and
  compare manually. Emit `benchmark.json` (see `references/schemas.md`)
  if you want a structured artefact, but there is no viewer that reads it
  inside AgentOS.
- **Optimize a description** → follow the manual loop in the main SKILL.md
  (Section "Description optimization"). Run the candidate descriptions
  through the actual triggering path 3× each, score, hand-improve.

### `present_files` tool

The upstream packaging step gates on `if present_files is available`. That
tool is a Claude.ai/Apps-specific UI hook for sending files back through
the chat UI. AgentOS does not have an equivalent — instead, write the
`.skill` file to disk and tell the user the path, or upload it through the
channel adapter the agent is running on.

### Sub-agents

AgentOS sub-agents are configured in `workspace/subagents/<name>.toml` and
selected via the `Plan::Delegate` arm of the run loop. Test cases run as
sub-agent invocations of the general subagent (or a purpose-built one)
rather than via the upstream's spawn-subagent task notification flow.

The sub-agent run result contains the full transcript, span set, and
final message in `RunState` — that's the equivalent of the upstream's
post-task `total_tokens` / `duration_ms` notification. Pull timing data
from the run's trace spans (`SpanKind::Run` start/end timestamps).

### Skill dispatch — deterministic planners vs LLM tool call

AgentOS supports two routes into a skill:

1. **`SkillPlanner` trait** — a Rust impl that inspects the incoming
   `RunContext` and returns a `Plan` deterministically. This is how
   `skill-creator` and `web-research` short-circuit common requests
   without paying for an LLM round-trip.
2. **`skill_create` (and similar) tools** — the LLM sees the tool in its
   schema and decides to call it.

Both routes share the same approval, guardrail, and trace flow. When you
build a new skill for AgentOS, prefer the LLM-tool route by default; only
add a `SkillPlanner` for prefix shortcuts that need to be free.

### Validation entry points

- **From the agent process**: `agentos skill validate <name>` (Rust). This
  is what the loader runs at startup and what `skill_create` runs before
  returning.
- **From the command line, outside the agent**: `python3 scripts/quick_validate.py <skill-path>`.
  Same shape checks plus the optional-field validation (`license`,
  `allowed-tools`, `metadata`, `compatibility`) that the Rust validator
  ignores.

### Workspace layout

`workspace/skills/` holds every workspace skill. The Rust loader scans
this directory at startup and indexes each subfolder as a skill resource.
The packaging script writes `<skill-name>.skill` to `cwd` by default —
typically you'd write it outside `workspace/skills/` so the bundle
doesn't get re-indexed on the next agent boot.

## Attribution

Workflow content, JSON schemas, and the Python script implementations
under `scripts/` are derived from the upstream Anthropic skill-creator
under its repository licence. Significant edits: removed the `claude`
CLI dependency, the eval-viewer server, the benchmark aggregation tooling,
and the `present_files` packaging branch; reframed sub-agent and
validation flows around AgentOS primitives.
