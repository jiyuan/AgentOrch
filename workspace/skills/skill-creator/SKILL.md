---
name: skill-creator
description: Create, update, validate, and package AgentOS workspace skills end-to-end. Use this whenever the user wants to scaffold a new skill, migrate or improve an existing one, optimize a skill description for triggering accuracy, build a distributable .skill bundle, or otherwise work with the SKILL.md folder format. Adapted from the upstream Anthropic skill-creator (https://github.com/anthropics/skills) for the AgentOS runtime.
---

# Skill Creator

A skill for creating, improving, and packaging AgentOS workspace skills.

At a high level, the loop is:

- Decide what the skill should do, when it should trigger, and roughly how it should accomplish the task.
- Write a draft `SKILL.md` and optional bundled resources.
- Run the skill against a small set of representative prompts and qualitatively review the outputs with the user.
- Improve the skill in response to the user's feedback.
- Repeat until the user is satisfied, then package the skill into a distributable `.skill` archive.

Your job when using this skill is to figure out where the user is in this loop and help them advance through it. Be flexible — if the user says "let's just vibe," skip the formal evaluation steps. If the user already has a draft, jump straight to running and evaluating it.

---

## Communicating with the user

Skill creators get used by people across a wide range of familiarity with software jargon. Pay attention to context cues. As a default heuristic:

- "Evaluation", "benchmark", "iteration" — borderline, but generally OK.
- "JSON", "assertion", "frontmatter" — wait for cues that the user is comfortable with those before using them without explanation.

When in doubt, briefly define a term the first time you use it. Match the user's register.

---

## Creating a skill

### Capture intent

Start by understanding what the user wants. The current conversation may already contain the workflow the user wants to capture ("turn this into a skill"). If so, extract the tools used, the sequence of steps, corrections the user made, and the input/output shapes observed. Ask the user to fill in any gaps and to confirm before scaffolding.

Specifically pin down:

1. What should this skill enable the agent to do?
2. When should it trigger? (What user phrasings / contexts?)
3. What does the expected output look like?
4. Are there inputs (files, URLs, structured data) it depends on?
5. Should we set up test cases? Skills with objectively verifiable outputs (file transforms, data extraction, code generation, fixed workflow steps) benefit from explicit test cases. Skills with subjective outputs (writing style, art) usually don't.

### Interview and research

Proactively ask about edge cases, example inputs, success criteria, and dependencies before writing test prompts. If MCPs are configured (search, docs lookup), use them in parallel to research conventions or similar existing skills.

### Write the SKILL.md

The local scaffold tool produces the skeleton, then you edit it. From inside the workspace agent:

```text
create skill: <skill-name>, <one-line description>, resources=scripts|references|assets
```

That deterministic prefix calls the built-in `skill_create` tool, which:

- Normalises the name to lowercase hyphen-case.
- Creates `workspace/skills/<skill-name>/SKILL.md` with valid frontmatter.
- Creates the requested resource subdirectories (any of `scripts`, `references`, `assets`).
- Runs `validate_skill_dir` before returning, so you only see success if the result parses.

Natural-language requests also work — the LLM sees the `skill_create` tool in its tool schema and can call it directly.

Once the skeleton exists, edit `SKILL.md` to fill in:

- **name** — Identifier in lowercase hyphen-case. Must match the directory name. Maximum 64 characters.
- **description** — Triggering signal. State both *what* the skill does and *specific contexts where it should be used*. Don't put "when to use" info in the body — put it here. Maximum 1024 characters, no `<` or `>`.
- **body** — Imperative, model-readable instructions. Keep it under ~500 lines; if it grows, push detail into `references/` and link from the body.

Optional frontmatter fields supported by the upstream spec (and accepted by `scripts/quick_validate.py`): `license`, `allowed-tools`, `metadata`, `compatibility`. AgentOS itself reads `name` and `description`; extra fields are preserved verbatim and ignored by the loader.

#### Description writing — leaning toward "pushy"

The description is the *only* signal that decides whether the skill triggers. The model has a tendency to under-trigger skills. Lean the description slightly toward over-triggering: instead of "How to build a fast dashboard for internal data," write "How to build a fast dashboard for internal data. Use this skill whenever the user mentions dashboards, data visualization, internal metrics, or wants to display company data of any kind, even if they don't explicitly say 'dashboard'."

You'll formally optimize this later — see [Description Optimization](#description-optimization).

### Skill anatomy

```
skill-name/
├── SKILL.md          # required: YAML frontmatter + body
├── scripts/          # optional: deterministic, repeatable scripts
├── references/       # optional: docs the model reads on demand
├── assets/           # optional: templates, fonts, icons, output collateral
└── evals/            # optional: evals.json + sample input files
```

#### Progressive disclosure

Skills load in three tiers:

1. **Metadata** (name + description) — always in context (~100 words).
2. **SKILL.md body** — in context whenever the skill triggers (target <500 lines).
3. **Bundled resources** — read on demand (`references/`), executed without reading (`scripts/`), or referenced in outputs (`assets/`).

Patterns that pay off:

- Reference bundled files explicitly from `SKILL.md` with guidance on when to read them.
- For long reference files (>300 lines), put a short table of contents at the top so the reader can jump.
- For multi-domain skills, organize by variant in `references/`:

```
cloud-deploy/
├── SKILL.md          # workflow + selection logic
└── references/
    ├── aws.md
    ├── gcp.md
    └── azure.md
```

The body says "for AWS, read `references/aws.md`" — only the relevant variant gets loaded.

### Writing patterns

Prefer the imperative form. Explain *why* something matters; today's models reason well when you give them theory of mind. Heavy-handed `MUST` and `NEVER` blocks are a yellow flag — usually the underlying reason will steer the model just as effectively and generalize better.

**Output template pattern:**

```markdown
## Report structure
Use this exact template:
# [Title]
## Executive summary
## Key findings
## Recommendations
```

**Example pattern:**

```markdown
## Commit message format
**Example:**
Input: Added user authentication with JWT tokens
Output: feat(auth): implement JWT-based authentication
```

### Principle of least surprise

Skills must not contain malware, exploit code, or anything that could compromise system security. The skill's behaviour should match what its description promises — no hidden side effects. Decline requests to create misleading skills or skills designed to facilitate unauthorized access. Roleplay-style skills are fine.

### Test cases

After the draft, draft 2-3 realistic test prompts — phrasings a real user would actually send. Share them with the user, confirm they're representative, then save to `evals/evals.json`. See `references/schemas.md` for the full schema.

```json
{
  "skill_name": "example-skill",
  "evals": [
    {
      "id": 1,
      "prompt": "User's task prompt",
      "expected_output": "Description of expected result",
      "files": []
    }
  ]
}
```

Don't draft formal assertions yet — write them while the runs are in progress in the next step.

---

## Running test cases on AgentOS

The AgentOS runtime does not include the upstream Anthropic eval-viewer server or `claude` CLI subprocess. Run test cases like this:

1. **Spawn each test prompt as a sub-agent task** if a sub-agent is configured (`agent.toml` / `workspace/subagents/`), or run them inline one at a time. Inline runs are fine for early iteration — the human review step compensates.
2. **Save outputs** under `workspace/skills/<skill-name>-workspace/iteration-<N>/eval-<ID>/with_skill/outputs/`. The workspace directory is a sibling of the skill directory, not inside it, so it never ships in the `.skill` bundle.
3. **Capture timing** from sub-agent completion notifications into `timing.json` (`total_tokens`, `duration_ms`). The notification is the only place this data is exposed.
4. **Grade with assertions.** Write a small grading script under `scripts/` for assertions that can be checked programmatically (string presence, file existence, structural shape). Save per-run results to `grading.json` with fields `text`, `passed`, `evidence` (see `references/schemas.md`).
5. **Present results to the user inline.** Show prompt + output for each test case. If outputs are files (`.docx`, `.csv`, images), save them to disk and tell the user the path. Ask for feedback: "How does this look? Anything you'd change?"

If the host environment has a browser and you want a visual review, you can write a simple static HTML page that lists prompts + outputs side by side — but don't depend on the upstream `generate_review.py`/`run_loop.py` Python tooling, which targets the Claude Code / Cowork environments specifically.

---

## Improving the skill

This is the heart of the loop. You've run the test cases, the user reviewed the outputs, now make the skill better.

### How to think about improvements

1. **Generalize from the feedback.** You're iterating against a handful of examples, but the skill will run against thousands of prompts you'll never see. Resist overfitty fixes ("if input contains 'foo' then…"). Instead, ask what general principle would have produced the right output. Try different metaphors and different patterns of work — it's cheap, and one of them may unlock a stubborn issue.
2. **Keep the prompt lean.** Remove what isn't pulling its weight. Read the transcripts, not just the final outputs — if the skill is making the model waste turns on unproductive sub-steps, delete the bits that caused that and see what happens.
3. **Explain the why.** Today's models have strong theory of mind. Instead of stacking `MUST` directives, explain the reasoning. If you find yourself writing `ALWAYS` or `NEVER` in caps, treat it as a yellow flag — usually you can reframe the same point as a reason the model will understand.
4. **Look for repeated work.** If every test case independently re-invented the same helper (`create_docx.py`, `build_chart.py`, …), bundle it under `scripts/` and point the skill at it. Write the helper once; save every future invocation from reinventing it.

### The iteration loop

1. Apply your improvements to `SKILL.md` (and any bundled resources).
2. Re-run the test cases into `iteration-<N+1>/`. If you're improving an existing skill, the baseline can be either the original or the previous iteration — use your judgement.
3. Present the new outputs to the user. If you can, show last iteration's output alongside the new one to make the delta obvious.
4. Read the new feedback, improve, repeat.

Stop when the user is happy, when feedback is empty, or when you stop making meaningful progress.

---

## Description optimization

After the skill works well, refine the description so the runtime triggers it correctly. The upstream `scripts/run_loop.py` automation depends on the `claude` CLI, which AgentOS does not ship — but the manual loop still applies:

### Step 1 — generate trigger eval queries

Create 20 queries: roughly 8-10 should trigger the skill, 8-10 should not. Save as JSON:

```json
[
  {"query": "the user prompt verbatim", "should_trigger": true},
  {"query": "an adjacent prompt that shouldn't fire", "should_trigger": false}
]
```

Queries must be concrete and realistic — file paths, column names, casual phrasing, abbreviations, typos. Bad example: `"Format this data"`. Good example: `"my boss just sent me a Q4 sales xlsx and wants a profit-margin column — revenue is column C and cost is column D i think"`.

For positives, cover different phrasings of the same intent (formal, casual), cases where the user doesn't name the skill explicitly, and edge cases where the skill competes with another but should win. For negatives, focus on near-misses — queries that share keywords with the skill but actually need something different. Obvious negatives ("write a fibonacci function" against a PDF skill) don't test anything.

### Step 2 — review with the user

Show the queries to the user, let them edit / toggle / add / remove. Bad eval queries lead to bad descriptions.

### Step 3 — manual optimization loop

For each candidate description:

1. Run each query through the actual triggering path 3 times (variability matters — single runs are noisy).
2. Score true-positive rate (positives that triggered) and true-negative rate (negatives that didn't).
3. Combine: `score = (true_positive_rate + true_negative_rate) / 2`.
4. Ask the model to propose a better description based on which queries failed and why, with the constraint that name + description remain under the 100-word "always in context" budget.

Run for up to 5 iterations, splitting the eval set 60/40 train/test. Pick the description with the best **test** score, not the best train score — that defends against overfitting to the train split.

### How triggering actually works

Skills appear to the runtime as a `name + description` pair. The runtime only consults a skill for tasks it can't easily handle directly, so trivial one-step queries ("read this PDF") often won't trigger any skill regardless of description quality. Tests should be substantive enough that the model would actually benefit from consulting a skill — simple lookups make poor test cases.

---

## Packaging and distribution

Once the skill is in good shape, package it for distribution:

```bash
python3 scripts/package_skill.py workspace/skills/<skill-name>
```

This validates the skill, then zips it into `<skill-name>.skill` (a renamed zip). Exclusions: `__pycache__/`, `node_modules/`, `*.pyc`, `.DS_Store`, and the top-level `evals/` directory (test cases stay out of the distribution bundle). The output file is suitable for upload to any host that accepts the Anthropic `.skill` format.

To unpack a `.skill` file someone else shares:

```bash
unzip <name>.skill -d workspace/skills/
```

---

## CLI utilities

AgentOS ships local commands that complement this skill:

```sh
agentos skill create <name> <description> --resources=scripts,references,assets
agentos skill validate <name>
agentos skill validate --all
agentos skill list
```

The Rust validator checks the same SKILL.md shape (frontmatter delimiters, required `name` + `description`, lowercase hyphen-case name, folder name matches `name`, non-empty body). Use `scripts/quick_validate.py` when you want the same checks plus optional-field validation outside the AgentOS process.

---

## Updating an existing skill

If the user wants to update rather than create:

- **Preserve the original name.** The skill's directory name and frontmatter `name` field stay unchanged. If they're updating `research-helper`, the packaged output is `research-helper.skill`, never `research-helper-v2.skill`.
- **Copy to a writeable location before editing** if the installed skill is read-only. Edit in `/tmp/<skill-name>/`, then package from the copy.
- **If packaging manually**, stage in `/tmp/` first and only move the `.skill` to the final destination after it's clean.

---

## Reference files

- `references/schemas.md` — JSON schemas for `evals.json`, `grading.json`, `history.json`, `benchmark.json`, `eval_metadata.json`, `feedback.json`. Useful when you write grading scripts or interoperate with the upstream Anthropic eval tooling.
- `references/agentos.md` — What differs between this port and the upstream Anthropic skill-creator. Read this when the upstream SKILL.md references a tool or capability that AgentOS does not have.

## Scripts

- `scripts/quick_validate.py` — Validates a SKILL.md folder. Same checks as the Rust validator plus optional-field checks. Exits non-zero on failure for use in pipelines.
- `scripts/package_skill.py` — Validates then zips a skill folder into a `.skill` archive, excluding build artifacts and test workspaces.

---

## The core loop, restated

- Figure out what the skill is about.
- Draft or edit the skill.
- Run the skill against representative prompts.
- With the user, evaluate the outputs.
- Repeat until the user is satisfied.
- Package the final skill (`.skill`) and hand it back.

Take your time and think things through. The user's time is the constraint; your thinking time is cheap. Write a draft, look at it with fresh eyes, improve it before showing it.
