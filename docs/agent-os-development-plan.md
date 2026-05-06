# Agent OS — Development Plan

A staged plan for building an agent-agnostic core engine in Rust, with a modifiable workspace and a swappable extensions shelf. This revision folds in orchestration and safety patterns from the OpenAI Agents SDK, adapted to Rust's type system and performance model.

---

## 1. What we keep from openai-agents-python — and what we change

The OpenAI SDK has been battle-tested across voice, sandbox, and multi-agent workflows. Several of its primitives are directly applicable; others need a different shape for a Rust, performance-first implementation.

### Primitives worth borrowing

**The single run loop.** One function drives everything: call LLM → if tool calls, execute them and loop; if text output and no tool calls, terminate. Multi-agent is not a separate mechanism — it's a handoff expressed as a tool call that swaps the active agent. This is the single most important simplifying idea in the SDK and Agent OS will adopt it directly. Your Orchestrator component is this loop.

**Handoffs as tools.** A handoff to `refund_agent` is emitted by the LLM as a `transfer_to_refund_agent` tool call. The runner intercepts it, swaps the active agent, and re-enters the loop. No parallel "routing system" — just a distinguished tool kind. This keeps the mental model small and makes sub-agent invocation and tool invocation share the same audit path.

**Guardrails at three scopes.** Input guardrails run once at the start (first agent only). Output guardrails run at the end (last agent only). Tool guardrails wrap individual function-tool calls. When any guardrail "trips," the run halts with a typed error. This is orthogonal to Approve — guardrails check *content*, Approve checks *permission*.

**Interruptions and resumable state.** When a tool needs human approval, the SDK doesn't block the run thread — it returns a `RunResult` with `interruptions` populated, and a serializable `RunState` that can be reconstituted. The caller approves or rejects out-of-band and resumes. This is how Approve should work in Agent OS: `ask_user` is a pause, not a blocking call.

**Lifecycle hooks.** `on_agent_start`, `on_llm_end`, `on_tool_start`, `on_tool_end`, `on_handoff`. Fixed lifecycle points with typed payloads give observability, tracing, and metrics a stable surface.

**Sessions layered above memory.** The SDK separates "conversation history" (Session) from "memory store" (backend). Sessions auto-load transcript before each run and auto-save after. This maps cleanly onto your workspace: `Session` is the transcript-management protocol, `Memory` is the storage backend.

**Tracing as a first-class citizen.** Every LLM call, tool call, handoff, and guardrail decision is a span nested under the run. Exporters are pluggable.

### What we change for Rust

**Typed state machine instead of implicit states.** The openai-agents loop uses runtime branching to decide what happens next. In Rust we express the loop states as an enum, and transitions as methods that consume and return variants. Invalid transitions become compile errors. The states (Start, Plan, Approve, Act, Observe, Finish, Paused) are shown in the diagram above this document.

**Sub-agents are isolated by the OS, not just by scope.** openai-agents runs sub-agents in the same process with a scoped context. For an OS-level platform, that is not enough — a misbehaving sub-agent can exhaust memory or hold locks. Agent OS runs sub-agents as separate tokio tasks by default, with an option to escalate to separate processes or WASM sandboxes when the policy demands it. Communication is via typed channels with bounded capacity.

**Backpressure everywhere.** Python's asyncio tolerates unbounded queues; Rust's tokio makes the choice explicit. Every mpsc channel in Agent OS has a bounded capacity. A slow LLM provider cannot accumulate a message backlog that OOMs the process.

**Zero-copy where it matters.** Tool schemas, LLM payloads, and trace spans are held as `Arc<...>` or `Bytes` and never deep-cloned on the hot path. `serde_json::RawValue` for tool arguments; avoid parsing JSON more than once.

**No dynamic dispatch inside the loop.** The Orchestrator, Memory, and Channel interfaces are traits — but inside the run loop, they are behind a single `&dyn` per trait. The loop itself is monomorphic. Extension swap happens at startup, not per call.

---

## 2. Crate layout

Workspace-style Cargo project. Each major component is its own crate so the core can evolve independently of channels, tools, and extensions.

```
agent-os/
├── Cargo.toml                      # workspace manifest
│
├── crates/
│   ├── agentos-core/               # The kernel
│   │   ├── loop/                   # RunLoop state machine
│   │   ├── gateway/                # Channel multiplexer
│   │   ├── orchestrator/           # Default orchestrator impl
│   │   ├── approve/                # Policy engine
│   │   ├── runtime/                # Tokio setup, tracing, error types
│   │   └── hooks/                  # Lifecycle hook dispatch
│   │
│   ├── agentos-interfaces/         # All public traits — the ABI of the system
│   │   ├── orchestrator.rs
│   │   ├── memory.rs
│   │   ├── session.rs
│   │   ├── channel.rs
│   │   ├── tool.rs
│   │   ├── skill.rs
│   │   ├── mcp.rs
│   │   ├── guardrail.rs
│   │   └── run_state.rs            # Serializable pause/resume state
│   │
│   ├── agentos-proto/              # Wire types shared across crates
│   │   └── src/                    # Message, Action, ToolCall, Event, Span
│   │
│   ├── agentos-llm/                # LLM client abstraction
│   │   └── providers/              # openai, anthropic, ollama adapters
│   │
│   └── agentos-cli/                # The binary
│
├── workspace/                      # Agent-owned content (not a Cargo crate)
│   ├── agent.toml                  # Agent definition + wiring
│   ├── memory/
│   ├── channels/                   # tui/, telegram/, feishu/ configs
│   ├── crons/
│   ├── subagents/
│   ├── skills/
│   ├── tools/                      # dynamic tools (WASM or scripts)
│   └── mcps/
│
└── extensions/                     # External crates that impl interfaces
    ├── orchestrators/              # e.g. plan-first, ReAct, graph-based
    └── memory/                     # e.g. sqlite, qdrant, redis
```

Two things to notice. **`agentos-interfaces` has no dependencies on anything else in the workspace** — it only defines traits and the associated types they need. This is the file you open to write an extension. **`agentos-proto` holds the wire types** (serializable via serde) — these cross process boundaries when sub-agents are isolated, so they cannot depend on any internal types.

---

## 3. Core interface sketches (Rust)

These are the trait shapes the Phase 0 work must produce. They are deliberately minimal — each can grow, but the initial versions freeze the method signatures.

```rust
// agentos-interfaces/src/orchestrator.rs
#[async_trait]
pub trait Orchestrator: Send + Sync {
    /// Given the current run state, decide the next action.
    /// Returns a Plan that the RunLoop will execute.
    async fn plan(&self, ctx: &RunContext) -> Result<Plan, OrchestratorError>;
}

pub enum Plan {
    Reply(Message),                  // terminal: emit and finish
    CallTool(ToolCall),              // non-terminal: execute tool, loop
    Handoff(AgentId, Option<Value>), // non-terminal: swap agent, loop
    Delegate(SubAgentSpec),          // non-terminal: spawn sub-agent, loop
}

// agentos-interfaces/src/memory.rs
#[async_trait]
pub trait Memory: Send + Sync {
    async fn write(&self, ns: &Namespace, record: Record) -> Result<RecordId>;
    async fn read(&self, ns: &Namespace, q: &Query) -> Result<Vec<Record>>;
    async fn forget(&self, ns: &Namespace, sel: &Selector) -> Result<usize>;
}

// agentos-interfaces/src/session.rs — layered above Memory
#[async_trait]
pub trait Session: Send + Sync {
    async fn load(&self, conv_id: &ConversationId) -> Result<Transcript>;
    async fn append(&self, conv_id: &ConversationId, items: Vec<Item>) -> Result<()>;
}

// agentos-interfaces/src/guardrail.rs
#[async_trait]
pub trait InputGuardrail: Send + Sync {
    async fn check(&self, input: &Input, ctx: &RunContext)
        -> Result<GuardrailOutcome>;
}
// Outcome { passed | tripped(reason) } — tripped halts the run with a typed error

// agentos-interfaces/src/channel.rs
#[async_trait]
pub trait Channel: Send + Sync {
    fn id(&self) -> ChannelId;
    async fn receive(&mut self) -> Option<Envelope>;
    async fn send(&self, env: Envelope) -> Result<()>;
}

// agentos-interfaces/src/run_state.rs
#[derive(Serialize, Deserialize)]
pub struct RunState {
    pub run_id: RunId,
    pub active_agent: AgentId,
    pub transcript: Transcript,
    pub pending_approvals: Vec<Interruption>,
    pub usage: Usage,
    pub version: SchemaVersion,
}
impl RunState {
    pub fn approve(&mut self, id: InterruptionId);
    pub fn reject(&mut self, id: InterruptionId, reason: String);
}
```

Approve is deliberately not a trait — it is a concrete engine that reads a declarative policy. This prevents the footgun of an extension "implementing Approve" in a way that unintentionally weakens the boundary.

---

## 4. The run loop as a typed state machine

```rust
pub enum RunLoopState {
    Start(StartCtx),
    Plan(PlanCtx),
    Approve(ApproveCtx),
    Act(ActCtx),
    Observe(ObserveCtx),
    Paused(RunState),        // serializable; round-trips to disk
    Finish(FinalOutput),
}

impl RunLoopState {
    pub async fn step(self, deps: &LoopDeps<'_>) -> Result<Self, RunError> {
        match self {
            Self::Start(c)    => start(c, deps).await,     // → Plan  (runs input guardrails)
            Self::Plan(c)     => plan(c, deps).await,      // → Approve | Finish
            Self::Approve(c)  => approve(c, deps).await,   // → Act | Paused | RunError
            Self::Act(c)      => act(c, deps).await,       // → Observe  (runs tool guardrails)
            Self::Observe(c)  => observe(c, deps).await,   // → Plan | Finish
            Self::Paused(s)   => Err(RunError::NotResumable),
            Self::Finish(_)   => Err(RunError::AlreadyDone),
        }
    }
}
```

`step()` consumes `self`. This is what makes invalid transitions impossible — you cannot call `observe()` on a `Plan` state because the types don't line up. The `max_turns` counter lives in `LoopDeps` and is checked in `plan()`.

**Where guardrails fire:** input in `start`, tool in `act`, output in `plan` when the result is a terminal `Plan::Reply`. The placement is not a convention — it is encoded in which state owns the check.

**Where hooks fire:** every transition emits a lifecycle event on the hooks bus. Hooks are fire-and-forget; they cannot alter the loop. (Anything that *should* alter the loop is a guardrail.)

---

## 5. Phased roadmap

Eight phases. Each ends with a demo that exercises the phase's additions end-to-end.

### Phase 0 — Foundations (1.5 weeks)

Set up the Cargo workspace. Pick dependencies deliberately: `tokio` for async, `tracing` + `tracing-subscriber` for observability, `serde` + `serde_json` for wire types, `thiserror` for errors, `async-trait` for the trait definitions. Add two CI checks: the import-boundary rule (nothing in `agentos-core` depends on `workspace/` — this is checked by scanning `Cargo.toml` and by running `cargo tree`), and a semver check on `agentos-interfaces` (once frozen, breaking changes fail CI).

Write every trait in `agentos-interfaces/`. No implementations. For each trait, write a `MockXxx` in a test-only module that stubs it out.

*Exit criterion:* `cargo check -p agentos-interfaces` passes; the import-boundary lint rejects a deliberately broken PR; documentation for each trait explains what the implementation must preserve.

### Phase 1 — Minimum viable loop (2 weeks)

Build the `RunLoopState` enum and its transitions. Wire one channel (TUI), a stub Orchestrator that returns `Plan::Reply` with an LLM echo, and an in-memory Memory + Session. Approve is a no-op policy. Guardrails list is empty. Hooks bus exists but has no subscribers.

Include `max_turns` from day one. It is trivial to add now and costly to retrofit after more logic lands in `plan()`.

*Exit criterion:* a user types in the terminal, the LLM replies. Trace shows one run, one Plan, one LLM span. The loop terminates within `max_turns` even when the LLM is adversarial (e.g. always emits a useless tool call).

### Phase 2 — Tools, Skills, and the first guardrails (2.5 weeks)

Implement the `Tool` trait and the registry. Three reference tools: `shell`, `http`, `file`. One reference Skill (`web-research`) that composes them. The Orchestrator now has to choose between `Plan::Reply` and `Plan::CallTool`.

Introduce input and output guardrails. Ship two reference guardrails: `PiiFilter` (input) and `MaxOutputLength` (output). Add tool guardrails — they run at entry and exit of `act()` — and ship one reference: `ShellCommandAllowlist`.

Important design choice: tools return a `ToolResult` that carries typed metadata (duration, tokens attributed, bytes of output), not just a string. This is what makes later observability meaningful.

*Exit criterion:* "summarize the top story on Hacker News" works. A run that tries a disallowed shell command is halted with a `GuardrailTripped` error that names the guardrail and the offending input.

### Phase 3 — Approve, interruptions, and resumable state (2 weeks)

Turn Approve from a no-op into a real policy engine. YAML policy language with three verbs: `allow`, `deny`, `ask_user`. Every action the orchestrator proposes passes through. An `ask_user` decision transitions the loop to `Paused` with a serialized `RunState`.

Implement `RunState` round-trip: serialize to disk, load, resume. The resumed run must produce the same trace as the un-paused run would have (modulo the approval delay). Write a test that does exactly this.

Wire `ask_user` back through the Gateway so the user sees an approval prompt on the same channel they are using. On approval or rejection, the run continues from where it paused — not from the beginning.

*Exit criterion:* a policy of "shell requires approval, read-only file access auto-allows, everything else denies" is enforceable. A run can pause for 24 hours and resume correctly, including if the process was restarted in between.

### Phase 4 — Persistent memory, sessions, crons, second channel (2.5 weeks)

Replace the in-memory store with a `sqlite` backend for Memory and a SQLite-backed Session. Add a small embedding index (use `hnsw_rs` or similar) for semantic recall. The embedding index is optional — the trait doesn't require it.

Add the cron subsystem. Scheduled tasks enter the loop as messages on the Gateway, indistinguishable from a user message. Reuse the existing envelope type — no special casing.

Add Telegram as the second channel. This is the real test of whether the Gateway abstraction holds: it should require zero changes in `agentos-core`.

*Exit criterion:* the agent remembers facts across restarts; a daily cron posts a summary to Telegram; the same trace shape is emitted regardless of channel origin.

### Phase 5 — Sub-agents and MCPs with real isolation (2.5 weeks)

Sub-agents are spawned as separate tokio tasks with their own channel pair for input/output. The parent's Approve policy is copied to the child and can only be *narrowed*, never widened. Enforce this in the spawn path: `Policy::narrow(&parent, &requested) -> Result<Policy>` returns an error if any rule would be more permissive.

MCP support exposes remote tools through the same `Tool` trait. The Orchestrator should not be able to tell whether a tool is local or MCP-backed — if it can, the abstraction has leaked.

For tools marked `requires_isolation` in the policy, escalate from tokio task to OS subprocess (unix domain socket transport) or WASM (using `wasmtime`). Start with subprocess — WASM can come later. The decision is declarative; the loop doesn't care.

*Exit criterion:* a parent agent spawns a research sub-agent with read-only web access. The parent has shell access, the child provably does not — a test proves that a child attempting shell access is denied at the Approve layer. MCP tools show up in traces with the same shape as local tools.

### Phase 6 — Extensions shelf and swap test (1 week)

Move the reference Orchestrator and Memory implementations into `extensions/` as template crates. Write one alternative Orchestrator (a plan-first variant that emits a plan artifact before acting) and one alternative Memory (Qdrant-backed). Switch between them by changing one line in `agent.toml`.

If this phase takes more than a week, the interfaces in Phase 0 were not really frozen. Treat a longer timeline as a signal to refactor the interfaces, not to push through.

*Exit criterion:* the same conversation under two different Orchestrators produces two coherent but different traces; the same data under two different Memory backends round-trips correctly.

### Phase 7 — Performance pass (1.5 weeks)

Benchmark the happy path. Target: **≤2 ms loop overhead per turn** excluding LLM latency and tool execution. Profile with `tokio-console` and `cargo flamegraph`. The common wins at this stage:

- Replace `String` clones with `Arc<str>` for agent names, tool names, channel ids.
- Use `serde_json::RawValue` for tool arguments — don't parse them until the tool needs them.
- Pool LLM HTTP clients (reqwest); one client per provider, not per request.
- `SmallVec<[_; 4]>` for guardrail lists (typically 0-3 entries).
- Batch trace span exports; don't flush per span.

Add load tests: 1000 concurrent conversations, measure tail latency. Fix anything that shows up.

*Exit criterion:* benchmark targets are met and published in `BENCHMARKS.md`. Memory usage is bounded — no leaks under 10k-turn stress test.

### Phase 8 — Hardening (ongoing)

Red-team the Approve policy language. Security review of the channel gateway (rate limiting, message size caps, authentication for non-TUI channels). Document every interface for extension authors. Publish `v1.0.0` of `agentos-interfaces` and enforce semver from then on.

---

## 6. Safety architecture — four concentric rings

Agent OS's safety story is that every dangerous operation passes through multiple, independent checks. The rings are:

**Ring 1 — Type system.** The `RunLoopState` enum means the loop cannot skip a state. `Plan` variants enumerate every possible action; adding a new kind of action requires updating the enum and — by compile error — every handler. `PolicyDecision` is not `bool`.

**Ring 2 — Guardrails (content).** Input, tool, and output guardrails inspect the data flowing through the loop. A tripwire halts the run with a typed error. Guardrails are cheap to add and agent-scoped.

**Ring 3 — Approve (permission).** Every action that reaches a boundary (tool call, sub-agent spawn, MCP invocation, cron schedule change) passes through the Approve engine. Approve can `allow`, `deny`, or `ask_user`. `ask_user` is a pause, not a block — the run serializes to `RunState` and resumes on decision.

**Ring 4 — OS isolation.** For actions marked `requires_isolation`, the work runs in a separate process or WASM sandbox. Even if a tool is compromised, it cannot reach the parent's filesystem or memory.

A sub-agent's permissions are the intersection of its parent's permissions and its own declared policy. Strictly narrowing, never widening, enforced at spawn.

---

## 7. Risks and how the plan addresses them

**Interface churn.** Breaking `agentos-interfaces` after Phase 0 breaks every extension. Mitigation: semver CI check from day one; additive changes only; a new major version only when genuinely unavoidable.

**Async cancellation correctness.** A cancelled tokio task that holds a Memory lock or a pending LLM request can leak. Mitigation: use structured concurrency (`tokio::task::JoinSet` with owned handles) and mark every external resource with a `Drop` impl that emits a cancellation trace. Test by aborting runs at each loop state and asserting clean shutdown.

**Sub-agent permission creep.** A parent that can spawn a child with equal permissions creates no isolation. Mitigation: `Policy::narrow` returns `Err` on any widening; a property test generates random parent/child policy pairs and verifies the invariant.

**Approve bypass via nested delegation.** A tool that calls an MCP server that invokes a shell command must still pass through Approve for the shell command. Mitigation: every tool call — including MCP-originated ones — re-enters the loop at the Approve state. No shortcut.

**Guardrail false-negatives on handoffs.** The openai-agents SDK only runs input guardrails for the first agent and output guardrails for the last. This can miss problems. Mitigation: Agent OS runs tool guardrails on *every* tool call regardless of which agent is active, including the synthetic `transfer_to_X` handoff tool. Opt-in "per-agent input guardrails" for cases where the first-agent-only behavior is desired.

**LLM provider coupling.** Building against one provider's quirks makes switching painful. Mitigation: `agentos-llm` is a thin trait (`Llm::complete(&self, req) -> Result<Response>`) with adapter crates. The Orchestrator talks to the trait, not to OpenAI or Anthropic directly.

**Tracing overhead on the hot path.** Naive `tracing` usage (format strings in every span) costs measurable latency. Mitigation: use structured fields, not formatted messages; sample debug-level spans; let the exporter, not the hot path, decide what to keep.

---

## 8. Concrete first steps (week one)

1. `cargo new --workspace agent-os` with the crate layout from §2.
2. Fill in `agentos-interfaces/` with every trait from §3. No implementations. Doc comments on every method.
3. Write a `MockOrchestrator`, `MockMemory`, `MockSession`, `MockChannel` in a `test-support` module.
4. Write the `RunLoopState` enum in `agentos-core/loop/` with empty `step()` bodies that `todo!()`.
5. Write a single integration test that drives `Start → Plan → Finish` with mock dependencies and asserts the trace shape.
6. Set up CI: `cargo check`, `cargo clippy -- -D warnings`, `cargo test`, import-boundary lint, `cargo semver-checks` on `agentos-interfaces`.
7. Write `DESIGN.md` explaining the loop state machine with the diagram. This is what external contributors read first.

Only after all seven of these are green does Phase 1 begin. The instinct to skip to a flashy demo is strong; the interfaces are the load-bearing part of the whole system.
