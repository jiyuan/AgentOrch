# AgentOS Memory System Development Plan

Status: draft
Source design: [`memory_system_design.md`](../memory_system_design.md)

This plan turns the memory-system design into an implementation sequence for the current AgentOS codebase. It assumes the existing Phase 4/5 baseline: SQLite-backed `Memory` and `Session`, `MemoryTool`, `RunContext.memory_fragments`, `Orchestrator::hydrate()`, `TaskWorkspace`, policy narrowing, sub-agent execution, and bounded gateway/run-loop architecture.

The plan keeps the core invariant intact: memory improves context assembly and persistence, but it does not own run-loop control flow. The run loop remains typed, Approve remains concrete, guardrails remain content checks, and storage remains behind the public `Memory` backend trait.

---

## 1. Goals

1. Add a scoped memory manager above the existing backend trait.
2. Make memory scopes explicit across user, agent, task, conversation, and shared memory.
3. Hydrate planning context through `Orchestrator::hydrate()` using `RunContext.memory_fragments`.
4. Route explicit memory mutation through the existing `memory` tool and policy path.
5. Add durable episode recording after finished runs without adding a new loop transition.
6. Give sub-agents opt-in, narrowed memory views.
7. Add auditability, retention hooks, and a migration path toward reflection and indexed retrieval.

Non-goals for the first delivery:

- Do not require embeddings or an external vector database.
- Do not replace `Memory` or `Session` traits.
- Do not split SQLite into separate files unless a later operational decision requires it.
- Do not allow child agents to inherit full parent memory by default.

---

## 2. Baseline Inventory

| Area | Current file(s) | Baseline behavior |
|---|---|---|
| Backend memory API | `crates/agentos-interfaces/src/memory.rs` | `write`, `read`, `forget` by `Namespace`. |
| Runtime memory backend | `crates/agentos-core/src/memory/mod.rs` | In-memory and SQLite implementations. |
| Session store | `crates/agentos-interfaces/src/session.rs`, `agentos-core/src/memory/mod.rs` | Conversation transcript load/append. |
| Planning context | `crates/agentos-interfaces/src/orchestrator.rs` | `RunContext.memory_fragments` exists but is not hydrated from storage. |
| Hydration hook | `agentos-core/src/orchestrator/max.rs` | `hydrate()` currently populates resources only. |
| Memory tool | `agentos-core/src/tools/memory.rs` | Raw namespace read/write/forget with no scoped access checks. |
| Policy | `agentos-core/src/approve/mod.rs` | Tool-level and argument-equality decisions are available. |
| Runtime wiring | `agentos-core/src/runtime/mod.rs` | CLI opens one `SqliteStore` and registers raw `MemoryTool`. |
| Task state | `agentos-core/src/task_workspace.rs` | Task metadata, state fragments, and JSONL session events. |
| Sub-agents | `agentos-core/src/subagents/mod.rs` | Policy narrowing and bounded child execution exist; memory view is implicit/absent. |

---

## 3. Delivery Strategy

Ship the system in six implementation milestones plus a hardening pass. Each milestone should be independently testable and should preserve current `remember:` / `recall:` behavior unless the milestone explicitly changes it behind tests.

Recommended branch flow:

1. `codex/memory-manager-scope`
2. `codex/memory-hydration`
3. `codex/scoped-memory-tool`
4. `codex/memory-episodes`
5. `codex/subagent-memory-views`
6. `codex/memory-reflection-indexing`

Do not combine milestones A through E into one large patch. They touch shared runtime surfaces and are easier to review with narrow exit criteria.

---

## 4. Milestone A: Scope Model and MemoryManager Skeleton

Purpose: introduce typed memory scope and caller authorization without changing existing backend traits or runtime behavior.

Primary files:

- `crates/agentos-core/src/memory/mod.rs`
- `crates/agentos-core/tests/start_plan_finish.rs`
- `crates/agentos-interfaces/src/orchestrator.rs` only if `MemoryFragment` needs additive metadata helpers.

Work items:

1. Add typed scope structures in `agentos-core::memory`:
   - `MemoryStore`
   - `MemoryOwner`
   - `MemoryVisibility`
   - `MemoryScope`
   - `MemoryCaller`
   - `HydrationRequest`
   - `RetrievalStrategy`
2. Implement deterministic scope-to-namespace conversion:
   - format: `{visibility}/{owner_kind}/{owner_id}/{store}/{domain}`
   - default domain: `general`
3. Add `MemoryManager` wrapping `Arc<dyn Memory>`.
4. Add authorization checks for:
   - caller-owned user/conversation memory;
   - caller agent-private memory;
   - caller task memory;
   - allowed shared domains;
   - audit/admin denial by default.
5. Add scoped `write_scoped`, `read_scoped` or `hydrate`, and `forget_scoped` skeletons using the existing backend.
6. Preserve raw `SqliteStore` behavior for existing direct backend tests.

Exit criteria:

- Scope namespace rendering is deterministic and covered by tests.
- Manager permits caller-owned scopes.
- Manager rejects cross-user, cross-agent, and cross-task reads.
- Manager permits shared semantic reads only for allowed domains.
- Existing `InMemoryMemory`, `SqliteStore`, and session tests still pass.

Validation:

```sh
cargo test -p agentos-core memory
cargo test -p agentos-core
sh scripts/check-import-boundaries.sh
```

---

## 5. Milestone B: SQLite Metadata, Audit Tables, and Access Accounting

Purpose: extend durable storage so scoped retrieval can filter efficiently and memory operations are auditable.

Primary files:

- `crates/agentos-core/src/memory/mod.rs`
- `crates/agentos-core/tests/start_plan_finish.rs`

Work items:

1. Extend SQLite initialization additively:
   - extra `memory_records` columns from the design document;
   - `memory_links`;
   - `memory_access_log`.
2. Keep migrations idempotent. Existing databases should open without manual reset.
3. On managed read:
   - update `last_accessed_at`;
   - increment `access_count`;
   - append a `memory_access_log` row.
4. On managed write/forget:
   - write scope fields as first-class columns and metadata;
   - append access-log rows.
5. Add tests that open an old-style database and verify schema extension succeeds.

Exit criteria:

- Existing SQLite memory records continue to round trip.
- Managed records persist scope columns and common metadata.
- Reads increment access count.
- Access log records operation, namespace, caller agent, task, conversation, and reason.

Validation:

```sh
cargo test -p agentos-core sqlite
cargo test -p agentos-core memory
```

---

## 6. Milestone C: Passive Hydration Through Orchestrator::hydrate

Purpose: make relevant memory available to planning through `RunContext.memory_fragments`, without changing the run-loop state machine.

Primary files:

- `crates/agentos-core/src/orchestrator/max.rs`
- `crates/agentos-core/src/runtime/mod.rs`
- `crates/agentos-core/src/loop/mod.rs` only for additive trace fields if needed.
- `crates/agentos-core/tests/start_plan_finish.rs`

Work items:

1. Give `MaxOrchestrator` an optional memory hydrator handle.
2. Build a `MemoryCaller` from `RunContext` and runtime envelope/session metadata.
3. Query allowed scopes using the latest user message.
4. Fill `ctx.memory_fragments` with compact `MemoryFragment` values.
5. Keep hydration budget small:
   - default max fragments: 5;
   - default max estimated tokens: 1200.
6. Extend hydrate tracing:
   - candidate count;
   - selected count;
   - namespace count;
   - fragment count remains available.
7. Make hydration configurable but default it off until Milestone C tests pass.

Exit criteria:

- A relevant stored record appears in `RunContext.memory_fragments` before planning.
- Hydration never returns records outside the caller view.
- Existing deterministic commands still behave the same.
- Hydrate trace reports non-zero fragments when memory is selected.

Validation:

```sh
cargo test -p agentos-core hydrate
cargo test -p agentos-core
cargo test -p agentos-cli cli_recalls_memory_across_processes
```

---

## 7. Milestone D: Scoped MemoryTool and Policy Tightening

Purpose: make explicit memory mutation safe by routing `MemoryTool` through `MemoryManager` and tightening policy around operation type.

Primary files:

- `crates/agentos-core/src/tools/memory.rs`
- `crates/agentos-core/src/tools/mod.rs`
- `crates/agentos-core/src/runtime/mod.rs`
- `crates/agentos-core/src/approve/mod.rs`
- `crates/agentos-core/src/orchestrator/commands.rs`

Work items:

1. Replace raw `Arc<dyn Memory>` in `MemoryTool` with a scoped manager or adapter.
2. Keep the tool schema compatible:
   - `operation`;
   - `namespace`;
   - `id`;
   - `body`;
   - `text`;
   - `limit`.
3. Add optional scope fields if needed:
   - `store`;
   - `owner`;
   - `visibility`;
   - `domain`.
4. Default unspecified writes from `remember:` into caller-owned semantic or episodic scope, not global `facts`.
5. Keep a backward-compatible read path for legacy `facts` records during migration.
6. Change reference policy:
   - allow `memory` reads;
   - ask user for `memory` writes;
   - ask user or deny `memory` forgets.
7. Add tests for malicious namespace requests.

Exit criteria:

- `remember:` / `recall:` still work across process restart.
- `MemoryTool` cannot read another caller's private namespace.
- Policy allows reads while requiring approval for writes/forgets.
- Tool result metadata includes operation, record id/count, namespace, and scope.

Validation:

```sh
cargo test -p agentos-core memory_tool
cargo test -p agentos-core approval
cargo test -p agentos-cli cli_recalls_memory_across_processes
```

---

## 8. Milestone E: Runtime Configuration

Purpose: make memory behavior configurable through `workspace/agent.toml` without making `agentos-core` depend on workspace contents.

Primary files:

- `crates/agentos-core/src/config.rs`
- `crates/agentos-core/src/runtime/mod.rs`
- `workspace/agent.toml`
- `crates/agentos-core/tests/start_plan_finish.rs`

Work items:

1. Add config structures:
   - `MemoryConfig`
   - `MemoryRetentionConfig`
   - `MemoryPolicyConfig`
   - `MemorySharedDomainConfig`
2. Parse:
   - backend;
   - path;
   - default domain;
   - hydration enabled;
   - hydrate fragment/token budgets;
   - retention budgets;
   - shared domain read/write policy.
3. Keep defaults compatible with current CLI behavior.
4. Wire config into `AgentRuntime::build()`.
5. Add validation:
   - unknown backend returns config error;
   - invalid budgets return config error;
   - shared domain names are normalized.

Exit criteria:

- `WorkspaceConfig::default()` remains valid.
- Existing `workspace/agent.toml` parses before and after adding `[memory]`.
- Runtime can turn hydration on/off from config.
- Config tests cover defaults and explicit memory settings.

Validation:

```sh
cargo test -p agentos-core config
cargo test -p agentos-cli
```

---

## 9. Milestone F: Post-Run Episode Recording

Purpose: persist selected run outcomes into episodic memory after a run finishes, without changing run-loop transitions.

Primary files:

- `crates/agentos-core/src/runner.rs`
- `crates/agentos-core/src/memory/mod.rs`
- `crates/agentos-core/tests/start_plan_finish.rs`

Work items:

1. Add optional memory manager field to `RunnerDeps`.
2. Call `MemoryManager::record_episode()` in `finish()` after:
   - transcript append;
   - task-session JSONL persistence;
   - trace persistence.
3. Record episodes for:
   - failures and denials;
   - approvals;
   - multi-step tool runs;
   - sub-agent workflows;
   - explicit user preference/correction;
   - explicit memory writes.
4. Skip trivial one-turn replies by default.
5. Store:
   - run id;
   - task id;
   - active agent;
   - conversation id;
   - outcome;
   - tools/sub-agents used;
   - compact summary.
6. Ensure episode recording failure does not corrupt the already-finished user response. Decide whether failure should be logged and returned as metadata, or surfaced as a runner error behind config.

Exit criteria:

- Multi-step tool run creates an episodic record.
- Denied/failed run creates an episode with failed/denied outcome.
- Simple echo run does not create an episode by default.
- Episode record includes provenance fields.

Validation:

```sh
cargo test -p agentos-core episode
cargo test -p agentos-core runner
```

---

## 10. Milestone G: Sub-Agent Memory Views

Purpose: give child agents controlled memory access without widening parent permissions.

Primary files:

- `crates/agentos-core/src/subagents/mod.rs`
- `crates/agentos-core/src/runtime/mod.rs`
- `crates/agentos-core/src/config.rs`
- `crates/agentos-core/src/orchestrator/routing.rs`
- `workspace/agent.toml`

Work items:

1. Add sub-agent config fields:
   - `memory_view`;
   - `memory_domains`;
   - optional `memory_tools`.
2. Carry memory-view descriptors through `SubAgentSpec.metadata`.
3. Convert child metadata into `MemoryCaller`.
4. Register scoped child `MemoryTool` only when enabled.
5. Enforce:
   - shared read-only memory for allowed domains;
   - child-private writes;
   - no parent-private reads;
   - no shared writes unless explicitly permitted.
6. Add tests proving child policy and memory view both narrow parent capability.

Exit criteria:

- Child can read allowed shared memory.
- Child cannot read parent private memory.
- Child writes land in child-private scope by default.
- Child cannot forget parent/user/shared memory unless explicitly allowed and approved.

Validation:

```sh
cargo test -p agentos-core subagent
cargo test -p agentos-core memory
```

---

## 11. Milestone H: Reflection, Retention, and Indexed Retrieval

Purpose: add long-horizon memory maintenance after the scoped, hydrated, and audited path is stable.

Primary files:

- `crates/agentos-core/src/memory/mod.rs`
- `crates/agentos-core/src/crons/mod.rs`
- `extensions/memory/`
- optional new tests under `crates/agentos-core/tests/`

Work items:

1. Add `ReflectionReport` and retention report types.
2. Implement reflection as a non-hot-path manager operation.
3. Add promotion path:
   - repeated episodes -> semantic facts;
   - repeated successful trajectories -> procedural candidates.
4. Add supersession handling:
   - mark stale semantic records `superseded`;
   - link replacement through `memory_links`.
5. Add archive/prune behavior by store budget.
6. Add optional lexical FTS index first.
7. Defer vector embeddings until deterministic lexical retrieval is validated.
8. Prepare extension boundary for alternative memory backends.

Exit criteria:

- Reflection can promote repeated episodes into one semantic fact with provenance links.
- Superseded facts stop appearing in default hydration.
- Pruning archives low-value records without breaking audit trail.
- Reflection can run from cron without channel or run-loop changes.

Validation:

```sh
cargo test -p agentos-core reflection
cargo test -p agentos-core crons
cargo test --workspace
```

---

## 12. Cross-Cutting Requirements

### Compatibility

- Existing `Memory` and `Session` traits remain source-compatible.
- `MemoryFragment` additions must be additive.
- Legacy `facts` namespace records should remain readable for at least one release.
- Existing CLI memory smoke tests must continue to pass.

### Safety

- Read-only hydration is constrained by manager authorization.
- Explicit memory mutation uses the tool path and Approve.
- Forget defaults to ask-user or deny.
- Sub-agent memory access only narrows parent access.
- Full memory bodies should not be written into trace fields.

### Observability

Add trace or audit coverage for:

- hydrate started/finished;
- candidate count;
- selected fragment count;
- managed write;
- managed forget;
- episode recorded/skipped;
- reflection started/finished.

### Performance

Initial budgets:

- hydrate max fragments: 5;
- hydrate max estimated tokens: 1200;
- query namespaces: caller-owned plus allowed shared domains only;
- no embedding call in hot path.

Hydration should remain bounded and deterministic. If query fan-out grows, add per-store limits before adding new retrieval strategies.

---

## 13. Test Matrix

| Area | Required tests |
|---|---|
| Scope | namespace canonicalization, default domain, invalid owner rejection. |
| Authorization | cross-user deny, cross-agent deny, cross-task deny, shared-domain allow. |
| SQLite | schema migration, metadata persistence, access count, audit rows. |
| Hydration | relevant fragment selected, irrelevant private fragment denied, budget enforced. |
| Tool | read allowed, write ask-user, forget ask-user/deny, malicious namespace denied. |
| Runtime config | defaults parse, explicit config parse, unknown backend error. |
| Episodes | tool run recorded, denied run recorded, trivial run skipped. |
| Sub-agents | shared read allowed, parent private denied, child private write. |
| Reflection | semantic promotion, supersession, archive/prune. |
| Regression | CLI recall across process, import boundary, workspace check/clippy. |

Standard verification before merging each milestone:

```sh
cargo fmt --all
cargo test -p agentos-core
cargo test -p agentos-cli
cargo check --workspace
cargo clippy --workspace -- -D warnings
sh scripts/check-import-boundaries.sh
```

Run `cargo semver-checks check-release -p agentos-interfaces` if any public interface changes.

---

## 14. Rollout Plan

1. Land Milestones A and B with hydration disabled.
2. Enable scoped reads internally for tests only.
3. Land Milestone C with hydration disabled by default in `workspace/agent.toml`.
4. Turn hydration on in local config after tests prove scope isolation.
5. Land Milestone D and migrate `remember:` / `recall:` to scoped behavior.
6. Add config controls in Milestone E.
7. Enable conservative episode recording in local config.
8. Add sub-agent views after parent memory scope is proven.
9. Add reflection and indexing only after manual recall/hydration quality is acceptable.

Rollback strategy:

- Disable hydration through config.
- Disable automatic episode recording through config.
- Keep raw backend records intact.
- Continue reading legacy `facts` namespace if scoped commands regress.

---

## 15. Open Decisions

Resolve before Milestone C:

- How to derive `user_id` for TUI and Telegram when envelope metadata lacks it.
- Whether `MemoryCaller` belongs only in `agentos-core` or should become an interface type later.
- Whether hydration should include agent-private semantic memory by default.

Resolve before Milestone D:

- Whether `remember:` writes semantic facts or episodic notes by default.
- Whether memory writes ask user in the CLI default policy or remain allowed for developer ergonomics.

Resolve before Milestone F:

- Whether episode recording failures are non-fatal logs or runner errors.
- What threshold defines a "non-trivial" run.

Resolve before Milestone H:

- Whether SQLite FTS is sufficient for first indexed recall.
- Which vector backend should be the first extension target if embeddings are added.

---

## 16. First Engineering Slice

The recommended first slice is Milestone A only:

1. Implement scope types and namespace rendering.
2. Implement manager authorization.
3. Add manager tests with `InMemoryMemory`.
4. Do not alter runtime wiring.
5. Do not alter `MemoryTool`.

This creates the core safety primitive before any automatic hydration or mutation behavior is enabled.

