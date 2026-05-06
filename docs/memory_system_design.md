# AgentOS Memory System Design

Status: proposed architecture after repository audit

This document designs a memory system that fits the current AgentOS framework instead of standing beside it. The design preserves AgentOS's core invariants: the run loop owns control flow, Approve remains concrete, guardrails stay separate from permission checks, public extension points live in `agentos-interfaces`, and the immutable `crates/` layer never depends on `workspace/` or `extensions/`.

The current implementation already has the right low-level seams:

- `agentos-interfaces::memory::Memory` stores records by `Namespace`.
- `agentos-interfaces::session::Session` stores conversation transcripts.
- `agentos-interfaces::orchestrator::RunContext` already has `memory_fragments`.
- `Orchestrator::hydrate()` is called before every `plan()`.
- `agentos-core::memory::SqliteStore` implements both `Memory` and `Session`.
- `agentos-core::tools::MemoryTool` exposes explicit user-visible memory operations through the ordinary tool path.
- `TaskWorkspace` persists task-local state, fragments, session events, and sub-orchestrator graphs.
- Sub-agent execution already narrows policy and uses bounded channels.

The missing layer is not another storage backend. It is a scoped memory manager that makes these pieces coherent: what should be remembered, where it belongs, who can read it, when it should hydrate planning context, and how it is pruned or promoted over time.

---

## 1. Architecture Audit

### Existing Memory Surfaces

| Surface | Current behavior | Design implication |
|---|---|---|
| `Memory` trait | Low-level `write`, `read`, `forget` by namespace. Query is text plus limit. | Keep as backend ABI. Do not overload it with orchestration, isolation, or summarization policy. |
| `Session` trait | Loads/appends ordered transcript items by `ConversationId`. | Treat session as conversation log, not semantic memory. It feeds episode creation and continuity. |
| `RunContext.memory_fragments` | Present but empty unless an orchestrator hydrates it. | This is the correct prompt-injection point for retrieved memory. |
| `Orchestrator::hydrate()` | Called and traced before planning. | Memory retrieval should happen here, not in the run loop's state machine. |
| `MemoryTool` | Explicit tool for `write`, `read`, `forget`; defaults to `facts`. | Keep for user/model-requested memory operations because it passes through Approve and tool guardrails. |
| `SqliteStore` | Stores JSON records and sessions in one SQLite database. | Good Phase 1 substrate. Extend schema rather than adding a parallel database immediately. |
| `TaskWorkspace` | Stores task state, fragments, and session JSONL under `workspace/tasks`. | Use for working memory checkpoints and long task state, not global recall. |
| `Policy` | Allows the `memory` tool broadly in `phase4_reference`. Supports argument equality matchers. | Narrow memory policy by operation/scope before enabling autonomous writes and forgets. |
| Sub-agents | Child runs use their own in-memory session and tool registry. Memory is not passed by default. | Add scoped memory views explicitly; never let children inherit the parent's full memory by accident. |
| Config | `workspace/agent.toml` has `agent.memory`, currently not selected by runtime. | Make memory backend and policies configurable here without making core depend on workspace contents. |

### Strengths

- The single typed run loop does not need to be redesigned for memory.
- `hydrate()` provides a deterministic place to attach memory to planning context.
- Existing traces already count hydrated memory fragments.
- Memory as a tool already uses the same approval, guardrail, trace, and transcript path as any other tool.
- SQLite persistence gives an immediate migration path and testable behavior across restarts.
- The task workspace already separates durable task state from global memory.

### Gaps

- Namespaces are free-form strings with no enforced user, agent, task, or visibility semantics.
- The same SQLite object is used as both session store and memory store, but session and memory retention policies are different.
- Hydration is not wired to the memory backend, so memory only appears when the user explicitly asks `recall:`.
- `MemoryTool` has no scoped access check. A caller that can invoke the tool can name any namespace.
- Sub-agents do not receive a permission-limited memory view.
- There is no episodic write path from completed runs, despite sessions and task JSONL containing the raw material.
- There is no reflection or compression path, so long-term memory would either bloat or stay manually curated.
- There is no access log for memory reads/writes/forgets, which weakens privacy and debugging.

---

## 2. Target Model

AgentOS should have three layers of memory:

1. **Session memory**: exact conversation transcript, owned by `Session`.
2. **Working memory**: task-local active context, owned by `RunState`, `RunContext.memory_fragments`, and `TaskWorkspace`.
3. **Long-term memory**: scoped records, owned by the `Memory` backend and mediated by a memory manager.

The manager is the only component that understands memory scope and routing. It composes the existing backend traits rather than replacing them.

```text
Gateway / Channel
      |
      v
Runner -> RunLoopState::Plan
      |          |
      |          v
      |    Orchestrator::hydrate()
      |          |
      |          v
      |    MemoryManager.retrieve_view()
      |          |
      |          v
      |    RunContext.memory_fragments
      |
      +-> Session append
      +-> TaskWorkspace session JSONL
      +-> post-run MemoryManager.record_episode()
```

Explicit model-initiated memory actions still use `Plan::CallTool("memory")`, which means they pass through:

```text
Plan -> Approve -> Act -> ToolGuardrails -> MemoryTool -> Observe
```

Automatic read-only hydration is handled by `Orchestrator::hydrate()` because it is part of planning context assembly. Automatic writes, compression, and reflection happen after task milestones through a memory manager with its own configured policy and audit log.

---

## 3. Store Types

The design uses the cognitive-store vocabulary, but maps it to AgentOS primitives.

| Store | AgentOS owner | Backing storage | Purpose |
|---|---|---|---|
| Working | `RunState`, `RunContext`, `TaskWorkspace` | In-memory state plus task `state.toml` and JSONL | Active task facts, selected observations, checkpoints, resume context. |
| Episodic | `Memory` backend | SQLite records; optional FTS/vector index | Completed run/session events with outcome and provenance. |
| Semantic | `Memory` backend | SQLite records plus links table; optional embedding/graph extension | Durable domain facts learned from episodes or explicit writes. |
| Procedural | Skills/resource registry plus `Memory` metadata | `workspace` skills/subagents plus indexed records | Repeatable workflows, skill success history, and trigger metadata. |
| Audit | `Memory` backend or dedicated SQLite table | Append-only operation log | Who read/wrote/forgot which memory and why. |

Sessions are intentionally excluded from the store list. A transcript is source material for memory, but exact chat history is not the same thing as long-term recall.

---

## 4. Scope and Isolation

### Scope Identity

The current `Namespace` type is a wrapper around `Arc<str>`. Keep it for backend compatibility, but stop treating it as arbitrary text. The memory manager should normalize namespace strings from a typed scope.

```rust
pub struct MemoryScope {
    pub store: MemoryStore,
    pub owner: MemoryOwner,
    pub visibility: MemoryVisibility,
    pub domain: Option<Arc<str>>,
}

pub enum MemoryStore {
    Working,
    Episodic,
    Semantic,
    Procedural,
    Audit,
}

pub enum MemoryOwner {
    User(Arc<str>),
    Agent(AgentId),
    Task(TaskId),
    Conversation(ConversationId),
    Shared,
}

pub enum MemoryVisibility {
    Private,
    Shared,
    Public,
}
```

The canonical namespace format should be deterministic:

```text
{visibility}/{owner_kind}/{owner_id}/{store}/{domain}
```

Examples:

```text
private/user/terminal/episodic/general
private/agent/main-agent/semantic/agentos
private/task/main/working/general
shared/shared/global/semantic/agentos
shared/shared/global/procedural/general
private/conversation/terminal/episodic/general
```

Until AgentOS has an explicit `UserId`, derive the user owner from `Envelope.metadata["user_id"]` when present and fall back to `ConversationId`. This keeps the design implementable without changing every channel at once.

### Caller View

Every memory read is evaluated against a caller view:

```rust
pub struct MemoryCaller {
    pub agent_id: AgentId,
    pub task_id: TaskId,
    pub conversation_id: ConversationId,
    pub user_id: Option<Arc<str>>,
    pub allowed_shared_domains: Vec<Arc<str>>,
}
```

The manager exposes only the union of:

- private memory for the active agent;
- task memory for the active task;
- conversation/user memory for the current caller;
- shared memory in allowed domains;
- explicitly delegated memory fragments from a parent run.

The manager rejects:

- user memory for a different user/conversation;
- private memory for another agent;
- task memory for another task unless the parent explicitly delegated it;
- procedural memory that is not enabled in the resource index;
- audit memory except through an administrative path.

### Sub-Agent Rules

Sub-agents default to a narrowed memory view:

- Can read shared semantic/procedural memory for the delegated domain.
- Can read task memory explicitly passed by the parent.
- Can write private episodic memory for the child agent.
- Cannot read parent private memory.
- Cannot write shared semantic/procedural memory directly unless the parent policy allows promotion.
- Cannot forget user/shared memory unless the parent policy and child policy both allow it.

In the current code, `SubAgentSpec.metadata` is the right place to carry the initial memory-view descriptor because it already survives approval serialization and child envelope creation. The child runtime should then convert that descriptor into a `MemoryCaller`.

---

## 5. API Shape

### Keep the Backend Trait Stable

The existing `Memory` trait remains the low-level backend ABI:

```rust
async fn write(&self, ns: &Namespace, record: Record) -> Result<RecordId, MemoryError>;
async fn read(&self, ns: &Namespace, q: &Query) -> Result<Vec<Record>, MemoryError>;
async fn forget(&self, ns: &Namespace, sel: &Selector) -> Result<usize, MemoryError>;
```

Do not add scope checks here. Backends should be dumb and replaceable.

### Add a Manager Above the Backend

Add a concrete manager in `agentos-core::memory` first. If it stabilizes, expose an additive trait in `agentos-interfaces` for external managers.

```rust
pub struct MemoryManager {
    backend: Arc<dyn Memory>,
    policy: MemoryPolicy,
    budgets: MemoryBudgets,
}

impl MemoryManager {
    pub async fn hydrate(
        &self,
        caller: &MemoryCaller,
        request: HydrationRequest,
    ) -> Result<Vec<MemoryFragment>, MemoryError>;

    pub async fn write_scoped(
        &self,
        caller: &MemoryCaller,
        scope: MemoryScope,
        body: Value,
        metadata: BTreeMap<Arc<str>, Value>,
    ) -> Result<RecordId, MemoryError>;

    pub async fn forget_scoped(
        &self,
        caller: &MemoryCaller,
        scope: MemoryScope,
        selector: Selector,
        reason: Arc<str>,
    ) -> Result<usize, MemoryError>;

    pub async fn record_episode(
        &self,
        caller: &MemoryCaller,
        state: &RunState,
        outcome: EpisodeOutcome,
    ) -> Result<Option<RecordId>, MemoryError>;

    pub async fn reflect_task(
        &self,
        caller: &MemoryCaller,
        task_id: &TaskId,
    ) -> Result<ReflectionReport, MemoryError>;
}
```

`hydrate()` returns `MemoryFragment` because that type already exists in the public orchestrator interface.

### Hydration Request

```rust
pub struct HydrationRequest {
    pub query: Arc<str>,
    pub domain: Option<Arc<str>>,
    pub max_fragments: usize,
    pub max_tokens: usize,
    pub stores: Vec<MemoryStore>,
    pub strategy: RetrievalStrategy,
}

pub enum RetrievalStrategy {
    Lexical,
    Recency,
    Hybrid,
}
```

Start with lexical plus recency because the current SQLite store already supports deterministic tests. Add FTS/vector retrieval as an internal backend improvement later.

---

## 6. Record Schema

Keep `Record.body` as JSON for extension flexibility, but standardize body and metadata keys.

### Common Metadata

All managed records should include:

```json
{
  "store": "episodic",
  "owner_kind": "user",
  "owner_id": "terminal",
  "visibility": "private",
  "domain": "agentos",
  "source_agent_id": "main-agent",
  "source_task_id": "main",
  "source_run_id": "cli-run-1",
  "conversation_id": "terminal",
  "importance": 0.0,
  "confidence": 1.0,
  "status": "active",
  "schema": "agentos.memory.v1"
}
```

Use metadata for filtering. Use `body` for content.

### Working Body

```json
{
  "kind": "working_fragment",
  "summary": "User asked to design AgentOS memory architecture.",
  "details": {},
  "expires_at_turn": 4
}
```

Working records should usually live in `TaskWorkspace.state.fragments`. Promote only durable checkpoints to the backend.

### Episodic Body

```json
{
  "kind": "episode",
  "event_type": "run_finished",
  "summary": "Designed AgentOS memory architecture.",
  "details": {
    "prompt": "...",
    "answer_summary": "...",
    "tools_used": []
  },
  "participants": ["main-agent"],
  "outcome": "succeeded"
}
```

Episode writes should be selective. Record failures, user corrections, approvals, new facts, and successful non-trivial workflows. Do not write a new episode for every ordinary echo or simple answer.

### Semantic Body

```json
{
  "kind": "semantic_fact",
  "subject": "AgentOS memory",
  "predicate": "hydrates_via",
  "object": "Orchestrator::hydrate",
  "summary": "Retrieved memory enters planning through RunContext.memory_fragments.",
  "source_record_ids": ["record-123"]
}
```

Semantic records are revisable. Do not overwrite contradicted facts. Mark the old record `status = "superseded"` and link to the replacement.

### Procedural Body

```json
{
  "kind": "procedure",
  "name": "memory_hydration_audit",
  "trigger_conditions": ["design memory system", "audit memory architecture"],
  "artifact": {
    "type": "skill_or_subagent",
    "path": "workspace/skills/memory_hydration_audit"
  },
  "success_count": 3,
  "failure_count": 0,
  "state": "probationary"
}
```

Procedural records should point to actual workspace artifacts rather than embedding large executable bodies in memory.

---

## 7. SQLite Layout

The current `memory_records` table is enough for Phase 1 compatibility:

```sql
memory_records(row_id, id, namespace, body_json, metadata_json, created_at)
```

Additive schema extension:

```sql
ALTER TABLE memory_records ADD COLUMN updated_at TEXT;
ALTER TABLE memory_records ADD COLUMN last_accessed_at TEXT;
ALTER TABLE memory_records ADD COLUMN access_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE memory_records ADD COLUMN status TEXT NOT NULL DEFAULT 'active';
ALTER TABLE memory_records ADD COLUMN store TEXT;
ALTER TABLE memory_records ADD COLUMN owner_kind TEXT;
ALTER TABLE memory_records ADD COLUMN owner_id TEXT;
ALTER TABLE memory_records ADD COLUMN visibility TEXT;
ALTER TABLE memory_records ADD COLUMN domain TEXT;
ALTER TABLE memory_records ADD COLUMN source_run_id TEXT;
ALTER TABLE memory_records ADD COLUMN source_task_id TEXT;
ALTER TABLE memory_records ADD COLUMN source_agent_id TEXT;
```

Add links and audit tables:

```sql
CREATE TABLE IF NOT EXISTS memory_links (
    row_id INTEGER PRIMARY KEY AUTOINCREMENT,
    from_id TEXT NOT NULL,
    to_id TEXT NOT NULL,
    relation TEXT NOT NULL,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_memory_links_from
    ON memory_links(from_id, relation);

CREATE TABLE IF NOT EXISTS memory_access_log (
    row_id INTEGER PRIMARY KEY AUTOINCREMENT,
    operation TEXT NOT NULL,
    record_id TEXT,
    namespace TEXT NOT NULL,
    caller_agent_id TEXT NOT NULL,
    caller_task_id TEXT,
    caller_conversation_id TEXT,
    reason TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
```

Optional retrieval indexes:

- SQLite FTS5 table for lexical search over summaries and body text.
- Vector side table or external vector backend for embeddings.
- Graph-style links through `memory_links` before adopting a dedicated graph database.

Do not require embeddings for the first implementation. The current roadmap already marks embedding recall as optional.

---

## 8. Runtime Integration

### Build-Time Wiring

Extend `AgentRuntime::build()`:

1. Open the configured memory backend.
2. Build a `MemoryManager` with policy and budgets from `workspace/agent.toml`.
3. Register `MemoryTool` as a scoped wrapper around the manager, not directly around the backend.
4. Give `MaxOrchestrator` a read-only hydrator handle.
5. Give sub-agent definitions scoped memory access only if their config enables the `memory` tool.

Current code always opens `SqliteStore` and then uses it for sessions and memory. The first implementation can keep that storage object, but split the meaning:

```text
SqliteStore as Session
SqliteStore as Memory backend
MemoryManager as policy/scope layer
```

### Hydration Path

Implement memory hydration in `MaxOrchestrator::hydrate()`:

```text
RunLoopState::Plan
  -> RunContext::from_state
  -> MaxOrchestrator::hydrate
  -> MemoryManager::hydrate
  -> ctx.memory_fragments = fragments
  -> MaxOrchestrator::plan
```

Hydration should use:

- latest user message as query;
- `ctx.system.task_id` as task scope;
- `ctx.system.active_agent` as agent scope;
- conversation/user identity from state metadata once available;
- a small fixed budget, for example 3 to 5 fragments and 800 to 1200 estimated tokens.

The plan span already records `memory_fragments`, so no trace model change is needed for initial visibility.

### Tool Path

Replace raw `MemoryTool` behavior with scoped behavior:

- `read`: allowed for caller view; returns summaries with record ids and provenance.
- `write`: allowed only to caller-owned or configured shared scopes.
- `forget`: deny by default; ask user for user-scoped memory; allow only administrative or explicit user-request paths.

Existing deterministic commands can map cleanly:

- `remember:` -> user/conversation semantic or episodic write.
- `recall:` -> hydration-style read from caller view.

Keep the existing command syntax as a smoke-test path, but do not build the design around it.

### Post-Run Episode Recording

Add a post-finish hook in `runner::finish()` after session append and task session persistence:

```text
record_run_finish
append transcript
persist task session items
persist trace
MemoryManager.record_episode(...)
```

This is not a new run-loop transition. It is persistence side effect after the run has already produced final output.

Episode recording policy should be conservative:

- Always record errors and guardrail/approval denials.
- Record successful multi-step tool/sub-agent workflows.
- Record explicit user preference or correction.
- Skip trivial one-turn replies unless the user explicitly asked to remember.

### Reflection

Reflection should run outside the hot path:

- after a task finishes if the number of new episodes crosses a small threshold;
- on a cron schedule;
- after detected contradictions;
- manually through an administrative tool.

Reflection can promote:

- repeated episode facts -> semantic records;
- repeated successful tool trajectories -> procedural candidates;
- old low-value episodes -> compressed summaries;
- contradicted facts -> superseded records.

---

## 9. Policy Model

Memory is a boundary action when the model requests it. Treat memory operations as policy-verifiable tool calls.

Recommended parent policy defaults:

```yaml
default: deny
rules:
  - tool: memory
    decision: allow
    args:
      operation: read
  - tool: memory
    decision: ask_user
    reason: memory write requires confirmation
    args:
      operation: write
  - tool: memory
    decision: ask_user
    reason: forgetting persistent memory requires confirmation
    args:
      operation: forget
```

The existing policy matcher can already match top-level arguments, so this works with the current `MemoryTool` shape. Later, add memory-specific policy fields only if operation-level matching is not enough.

Read-only hydration is not a tool call. It must be constrained by the memory manager's caller-view checks and should never return records outside the caller's allowed view.

---

## 10. Configuration

Extend `workspace/agent.toml` with a memory section:

```toml
[memory]
backend = "sqlite"
path = "agentos.sqlite"
default_domain = "general"
hydrate = true
hydrate_max_fragments = 5
hydrate_max_tokens = 1200

[memory.retention]
episodic_max_records = 10000
semantic_max_records = 50000
procedural_max_records = 1000

[memory.policy]
auto_record_episodes = true
auto_promote_semantic = false
shared_writes = "ask_user"
forget_user_memory = "ask_user"

[[memory.shared_domains]]
name = "agentos"
read = true
write = false
```

Sub-agent config should opt into memory explicitly:

```toml
[[subagents]]
id = "research-subagent"
policy_id = "readonly-web"
tools = ["http"]
memory_view = "shared_readonly"
memory_domains = ["agentos"]
```

If a sub-agent includes `tools = ["memory"]`, runtime construction must register a scoped memory tool for that child and the parent policy must still narrow successfully.

---

## 11. Retrieval Strategy

Phase 1 retrieval should be deterministic:

1. Build candidate scopes from the caller view.
2. Query each allowed namespace using lexical matching and recency.
3. Filter inactive/superseded records unless explicitly requested.
4. Score candidates:
   - exact lexical match;
   - same task/domain;
   - importance;
   - recency;
   - access count;
   - confidence.
5. Return compact `MemoryFragment`s with provenance metadata.
6. Increment `access_count` and append an audit log row.

Suggested scoring:

```text
score =
  3.0 * exact_match
+ 2.0 * domain_match
+ 1.5 * recency_bucket
+ 1.0 * importance
+ 0.8 * confidence
+ 0.4 * log1p(access_count)
- 2.0 * superseded_penalty
```

Future hybrid retrieval can add embeddings without changing the manager contract.

---

## 12. Compression and Retention

Retention should be per store:

| Store | Default retention | Pruning action |
|---|---|---|
| Working | Task duration or explicit checkpoint | Evict from context, keep task state summary. |
| Episodic | Budgeted by record count and importance | Compress into session/task summaries, then mark archived. |
| Semantic | Long-lived while active/confident | Supersede on contradiction, prune low-confidence stale facts. |
| Procedural | Based on success/failure history | Demote probationary skills, archive failed routines. |
| Audit | Append-only for configured window | Rotate/export, do not silently rewrite. |

Forget semantics are different from prune semantics:

- **Forget** is an explicit deletion/suppression request, usually user-initiated.
- **Prune** is budget management.
- **Supersede** preserves provenance while replacing stale knowledge.
- **Archive** removes records from default retrieval without deleting them.

User deletion requests should mark matching records deleted and remove them from retrieval immediately. If hard deletion is required by deployment policy, the backend can physically remove them after logging the forget request.

---

## 13. Observability

Memory operations should emit:

- trace event fields for hydrate count, candidate count, selected count, and namespaces touched;
- audit log rows for reads, writes, forgets, promotions, and prunes;
- tool result metadata for explicit memory calls;
- task workspace events when memory fragments influence a task.

Do not put full memory contents in trace fields. Use record ids, namespaces, counts, and summaries only.

Recommended trace events:

```text
memory_hydrate_started
memory_hydrate_finished
memory_record_written
memory_record_forgotten
memory_reflection_started
memory_reflection_finished
```

---

## 14. Implementation Plan

### Phase A: Scope and Manager Skeleton

Files:

- `crates/agentos-core/src/memory/mod.rs`
- `crates/agentos-interfaces/src/orchestrator.rs`
- `crates/agentos-core/tests/start_plan_finish.rs`

Work:

- Add typed scope helpers in `agentos-core::memory`.
- Add `MemoryManager` that wraps `Arc<dyn Memory>`.
- Convert scope to canonical `Namespace`.
- Add caller-view authorization tests.
- Keep the public `Memory` trait unchanged.

Exit criteria:

- Unauthorized cross-owner reads/writes are rejected by manager tests.
- Existing memory and session tests still pass.

### Phase B: Hydration

Files:

- `crates/agentos-core/src/orchestrator/max.rs`
- `crates/agentos-core/src/runtime/mod.rs`
- `crates/agentos-core/tests/start_plan_finish.rs`

Work:

- Give `MaxOrchestrator` an optional memory hydrator.
- Populate `RunContext.memory_fragments` in `hydrate()`.
- Add trace fields for candidate and selected fragment counts.
- Keep deterministic command routing unchanged.

Exit criteria:

- A stored fact relevant to the latest user message appears in `RunContext.memory_fragments`.
- The existing hydrate trace span reports non-zero fragments.

### Phase C: Scoped Memory Tool

Files:

- `crates/agentos-core/src/tools/memory.rs`
- `crates/agentos-core/src/runtime/mod.rs`
- `crates/agentos-core/src/approve/mod.rs`

Work:

- Route tool calls through `MemoryManager`.
- Enforce caller view on namespace arguments.
- Use policy argument matching for `read`, `write`, and `forget`.
- Return record ids and scope metadata in tool results.

Exit criteria:

- `remember:` and `recall:` still pass across process restart.
- Attempting to read another caller's namespace fails.
- Forget defaults to ask-user or deny depending on policy.

### Phase D: Episode Recording

Files:

- `crates/agentos-core/src/runner.rs`
- `crates/agentos-core/src/memory/mod.rs`
- `crates/agentos-core/tests/start_plan_finish.rs`

Work:

- Add optional `memory_manager` to `RunnerDeps`.
- Record selected episodes after finished runs.
- Include task id, run id, agent id, conversation id, outcome, and summarized transcript/tool usage.
- Do not record trivial runs unless explicit memory write occurred.

Exit criteria:

- Multi-step tool run creates an episodic record.
- Failed/denied run creates an episode with outcome `failed` or `denied`.
- Simple echo run does not create long-term memory by default.

### Phase E: Sub-Agent Memory Views

Files:

- `crates/agentos-core/src/subagents/mod.rs`
- `crates/agentos-core/src/runtime/mod.rs`
- `crates/agentos-core/src/config.rs`

Work:

- Add memory view metadata to sub-agent invocation.
- Register scoped child memory tools only when config enables them.
- Ensure child memory policy is narrowed with parent policy.
- Add tests for shared-read, parent-private-denied, and child-private-write behavior.

Exit criteria:

- Child can read allowed shared memory.
- Child cannot read parent private memory.
- Child memory writes land in child-private scope unless explicitly promoted.

### Phase F: Reflection, Compression, and Indexing

Files:

- `crates/agentos-core/src/memory/mod.rs`
- `crates/agentos-core/src/crons/mod.rs`
- `extensions/memory/`

Work:

- Add reflection reports and promotion logic.
- Add memory links table.
- Add optional FTS or embedding index behind backend capability detection.
- Move alternative memory implementations into `extensions/memory` when the extension shelf is ready.

Exit criteria:

- Repeated episodes can promote one semantic fact with provenance links.
- Superseded semantic facts stop appearing in default hydration.
- Reflection can run from cron without changing channel or run-loop code.

---

## 15. Test Plan

Minimum tests before enabling automatic hydration by default:

- `MemoryScope` canonical namespace round trip.
- Manager rejects cross-user, cross-agent, and cross-task reads.
- Manager permits shared semantic reads for allowed domains.
- Hydration populates `RunContext.memory_fragments` before planning.
- `MemoryTool` enforces scope even when a malicious namespace is supplied.
- Policy can allow reads while asking for writes and forgets.
- Episode recording creates records only for selected outcomes.
- Sub-agent memory view cannot widen parent memory permissions.
- SQLite records persist across reopen with metadata and access counts.
- Import-boundary script still passes.
- `cargo semver-checks` passes if public interfaces change.

Manual smoke tests:

```sh
cargo test -p agentos-core
cargo test -p agentos-cli
cargo check --workspace
cargo clippy --workspace -- -D warnings
sh scripts/check-import-boundaries.sh
```

---

## 16. Decisions

Decisions made by this design:

- Keep `Memory` as backend ABI and place isolation in a manager layer.
- Use `Orchestrator::hydrate()` for passive retrieval.
- Use `MemoryTool` for explicit model/user-requested memory mutation.
- Use SQLite as the first durable substrate.
- Use deterministic namespace conventions before requiring new proto id types.
- Treat sub-agent memory as opt-in and narrowed.
- Start with lexical/recency retrieval; add embeddings later.

Open decisions:

- Whether to add first-class `UserId` to `agentos-proto`.
- Whether to split session and memory into separate SQLite files by default.
- Whether automatic episode recording should be enabled by default in the CLI.
- Which embedding/vector backend should become the first extension implementation.
- How much memory access logging is required for the target deployment threat model.

