# JSON schemas

Reference shapes for the JSON files this skill produces and consumes. Adapted
from the upstream Anthropic [skill-creator schemas](https://github.com/anthropics/skills/blob/main/skills/skill-creator/references/schemas.md);
fields the AgentOS port does not currently read are marked **Upstream-only**.

## Table of contents

- [evals.json](#evalsjson)
- [eval_metadata.json](#eval_metadatajson)
- [timing.json](#timingjson)
- [grading.json](#gradingjson)
- [feedback.json](#feedbackjson)
- [benchmark.json](#benchmarkjson) (Upstream-only)
- [history.json](#historyjson) (Upstream-only)

---

## evals.json

Test cases for a skill. Lives at `evals/evals.json` inside the skill folder.
The packager excludes this directory from the `.skill` bundle by default.

```json
{
  "skill_name": "example-skill",
  "evals": [
    {
      "id": 1,
      "prompt": "User's task prompt",
      "expected_output": "Description of expected result",
      "files": ["evals/files/sample1.pdf"],
      "expectations": [
        "The output includes X",
        "The skill used script Y"
      ]
    }
  ]
}
```

**Fields:**

- `skill_name` — matches the skill's frontmatter `name`.
- `evals[].id` — unique integer.
- `evals[].prompt` — the task prompt as a user would type it.
- `evals[].expected_output` — human-readable success criterion.
- `evals[].files` — optional input file paths relative to the skill root.
- `evals[].expectations` — verifiable statements. Drafted while runs are in progress; can be empty in the initial commit.

---

## eval_metadata.json

Per-run metadata. Lives at `<workspace>/iteration-<N>/eval-<ID>/eval_metadata.json`.

```json
{
  "eval_id": 0,
  "eval_name": "descriptive-name-here",
  "prompt": "The user's task prompt",
  "assertions": []
}
```

`eval_name` should describe what the test exercises (`"multi-page-pdf-fill"`,
not `"eval-0"`). Re-emit per iteration when prompts or assertions change —
don't assume metadata carries forward from a previous iteration.

---

## timing.json

Wall-clock data for a single run. Lives at `<run-dir>/timing.json`.

**How to capture:** when a sub-agent task completes, its notification carries
`total_tokens` and `duration_ms`. That notification is the *only* place this
data is exposed — save it immediately.

```json
{
  "total_tokens": 84852,
  "duration_ms": 23332,
  "total_duration_seconds": 23.3,
  "executor_start": "2026-05-12T10:30:00Z",
  "executor_end": "2026-05-12T10:32:45Z",
  "executor_duration_seconds": 165.0,
  "grader_start": "2026-05-12T10:32:46Z",
  "grader_end": "2026-05-12T10:33:12Z",
  "grader_duration_seconds": 26.0
}
```

---

## grading.json

Output from the grader pass. Lives at `<run-dir>/grading.json`. The viewer
(when present) reads `expectations[].text`, `expectations[].passed`, and
`expectations[].evidence` by those exact names — using other names breaks
downstream tooling.

```json
{
  "expectations": [
    {
      "text": "The output includes the name 'John Smith'",
      "passed": true,
      "evidence": "Found in transcript Step 3: 'Extracted names: John Smith, Sarah Johnson'"
    },
    {
      "text": "The spreadsheet has a SUM formula in cell B10",
      "passed": false,
      "evidence": "No spreadsheet was created. The output was a text file."
    }
  ],
  "summary": {
    "passed": 1,
    "failed": 1,
    "total": 2,
    "pass_rate": 0.50
  },
  "execution_metrics": {
    "tool_calls": {"Read": 5, "Write": 2, "Bash": 8},
    "total_tool_calls": 15,
    "total_steps": 6,
    "errors_encountered": 0,
    "output_chars": 12450,
    "transcript_chars": 3200
  },
  "timing": {
    "executor_duration_seconds": 165.0,
    "grader_duration_seconds": 26.0,
    "total_duration_seconds": 191.0
  },
  "claims": [
    {
      "claim": "The form has 12 fillable fields",
      "type": "factual",
      "verified": true,
      "evidence": "Counted 12 fields in field_info.json"
    }
  ],
  "user_notes_summary": {
    "uncertainties": ["Used 2023 data, may be stale"],
    "needs_review": [],
    "workarounds": ["Fell back to text overlay for non-fillable fields"]
  },
  "eval_feedback": {
    "suggestions": [
      {
        "assertion": "The output includes the name 'John Smith'",
        "reason": "A hallucinated document that mentions the name would also pass"
      }
    ],
    "overall": "Assertions check presence but not correctness."
  }
}
```

`execution_metrics`, `claims`, `user_notes_summary`, and `eval_feedback` are
optional. Include them when the grader produces them; the minimum shape is
`expectations[]` + `summary`.

---

## feedback.json

Captures the user's qualitative review of an iteration. The upstream viewer
writes this to a download; on AgentOS you can ask the user inline and write
the JSON yourself.

```json
{
  "reviews": [
    {"run_id": "eval-0-with_skill", "feedback": "the chart is missing axis labels", "timestamp": "2026-05-12T18:11:02Z"},
    {"run_id": "eval-1-with_skill", "feedback": "", "timestamp": "2026-05-12T18:11:14Z"},
    {"run_id": "eval-2-with_skill", "feedback": "perfect, love this", "timestamp": "2026-05-12T18:11:23Z"}
  ],
  "status": "complete"
}
```

Empty feedback strings mean the user was satisfied. Focus the next iteration
on the runs that drew specific complaints.

---

## benchmark.json (Upstream-only)

Quantitative summary across with-skill and without-skill runs, produced by
the upstream `scripts/aggregate_benchmark.py`. The aggregator and viewer are
not part of this port — the schema is preserved here so AgentOS scripts that
interoperate with the upstream viewer can emit the right shape.

```json
{
  "metadata": {
    "skill_name": "pdf",
    "skill_path": "/path/to/pdf",
    "executor_model": "claude-sonnet-4-6",
    "timestamp": "2026-05-12T10:30:00Z",
    "evals_run": [1, 2, 3],
    "runs_per_configuration": 3
  },
  "runs": [
    {
      "eval_id": 1,
      "eval_name": "Ocean",
      "configuration": "with_skill",
      "run_number": 1,
      "result": {
        "pass_rate": 0.85,
        "passed": 6,
        "failed": 1,
        "total": 7,
        "time_seconds": 42.5,
        "tokens": 3800,
        "tool_calls": 18,
        "errors": 0
      },
      "expectations": [
        {"text": "...", "passed": true, "evidence": "..."}
      ]
    }
  ],
  "run_summary": {
    "with_skill": {
      "pass_rate": {"mean": 0.85, "stddev": 0.05, "min": 0.80, "max": 0.90},
      "time_seconds": {"mean": 45.0, "stddev": 12.0, "min": 32.0, "max": 58.0},
      "tokens": {"mean": 3800, "stddev": 400, "min": 3200, "max": 4100}
    },
    "without_skill": {
      "pass_rate": {"mean": 0.35, "stddev": 0.08, "min": 0.28, "max": 0.45},
      "time_seconds": {"mean": 32.0, "stddev": 8.0, "min": 24.0, "max": 42.0},
      "tokens": {"mean": 2100, "stddev": 300, "min": 1800, "max": 2500}
    },
    "delta": {
      "pass_rate": "+0.50",
      "time_seconds": "+13.0",
      "tokens": "+1700"
    }
  },
  "notes": [
    "Without-skill runs consistently fail on table extraction expectations"
  ]
}
```

The viewer reads `configuration` (literally `"with_skill"` or `"without_skill"`),
`result.pass_rate`, and the nested `mean`/`stddev` fields verbatim — synonyms
break it.

---

## history.json (Upstream-only)

Tracks version progression during skill improvement. Produced by the upstream
improve loop; preserved here for parity. Lives at the workspace root.

```json
{
  "started_at": "2026-05-12T10:30:00Z",
  "skill_name": "pdf",
  "current_best": "v2",
  "iterations": [
    {"version": "v0", "parent": null, "expectation_pass_rate": 0.65, "grading_result": "baseline", "is_current_best": false},
    {"version": "v1", "parent": "v0", "expectation_pass_rate": 0.75, "grading_result": "won", "is_current_best": false},
    {"version": "v2", "parent": "v1", "expectation_pass_rate": 0.85, "grading_result": "won", "is_current_best": true}
  ]
}
```

`grading_result` is one of `"baseline"`, `"won"`, `"lost"`, `"tie"`.
