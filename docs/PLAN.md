# Agent OS Development Progress

Last synced: 2026-04-17

## Current Phase

Phase 5 is started. Phase 0 is complete for the local scaffold, Phase 1's local CLI demo is working with trace validation, Phase 2 exit criteria are met locally, Phase 3's approval/resume path has durable-session coverage, and Phase 4's persistence, cron ingress, and Telegram channel criteria are met locally.

## Phase 0 — Foundations

Status: complete for local development.

- Cargo workspace exists with `agentos-proto`, `agentos-interfaces`, `agentos-core`, `agentos-llm`, and `agentos-cli`.
- Core dependencies are in place: `tokio`, `tracing`, `tracing-subscriber`, `serde`, `serde_json`, `thiserror`, and `async-trait`.
- `agentos-interfaces` defines public traits for orchestrator, memory, session, channel, tool, skill, MCP, guardrails, and run state.
- Test-support mocks exist for every public interface trait.
- Import-boundary check exists in `scripts/check-import-boundaries.sh` and passes locally.
- CI runs check, clippy, tests, import-boundary lint, and PR semver checks for `agentos-interfaces`.
- `cargo semver-checks check-release -p agentos-interfaces --baseline-rev main` passes locally: 196 checks passed, 56 skipped, and no semver update was required.
- `DESIGN.md` documents the loop state machine and safety rings.

Remaining validation:

- Add a deliberately broken import-boundary fixture or CI job if we want an automated negative test rather than relying on the scanner itself.

## Phase 1 — Minimum Viable Loop

Status: demo working locally; ready for Phase 1 cleanup/exit review.

Completed:

- `RunLoopState` exists with typed `Start`, `Plan`, `Approve`, `Act`, `Observe`, `Paused`, and `Finish` states.
- `step()` consumes state and returns the next typed state.
- `max_turns` is checked in planning.
- Integration test covers `Start -> Plan -> Finish`.
- Adversarial tool-loop test verifies `MaxTurnsExceeded`.
- Reference in-memory `Memory` and `Session` implementations exist in `agentos-core`.
- Hooks bus and gateway scaffolds exist and use bounded Tokio channels.
- Stub `EchoOrchestrator` returns `Plan::Reply`.
- `agentos-core::runner::run_envelope` drives the state machine until `Finish`, `Paused`, or error.
- Runner loads transcript from `Session`, adds the inbound message for planning, and appends inbound + assistant messages back to the in-memory session after a finished run.
- Loop planning records deterministic trace spans for one run, one plan step, and one LLM-equivalent orchestrator call.
- `agentos-cli` includes a minimal TUI `Channel` implementation, accepts one terminal prompt, runs it through the echo orchestrator, prints the echo reply, and reports the trace shape.
- Local verification passes: `cargo test -p agentos-core`, `cargo check --workspace`, `cargo clippy --workspace -- -D warnings`, `sh scripts/check-import-boundaries.sh`, and a CLI smoke run.

Next work:

- Decide whether Phase 1 needs an automated negative import-boundary fixture before exit, or whether that remains Phase 0 follow-up.

Exit criterion target:

- A user types in the terminal and receives an echo reply. Locally verified with `printf 'hello phase 1\n' | cargo run -p agentos-cli`.
- Trace includes one run, one plan step, and one LLM-equivalent span. Locally verified as `trace: run=1, plan=1, llm=1`.
- The loop terminates within `max_turns` for an adversarial orchestrator.

## Phase 2 — Tools, Skills, and First Guardrails

Status: exit criteria met locally; ready for Phase 2 cleanup/exit review.

Completed:

- `agentos-core::tools::ToolRegistry` exists and dispatches registered `Tool` implementations by tool name.
- Reference `shell`, `http`, and `file` tools exist in `agentos-core::tools`.
- Tool results carry metadata such as `duration_ms`, `bytes_out`, exit status, and status line where applicable.
- The run loop executes `Plan::CallTool` in `Act`, appends tool results into the transcript, emits a tool span, and returns to `Observe -> Plan`.
- Input, output, and tool guardrail hooks are wired into `Start`, terminal `Plan::Reply`, and `Act`.
- Reference `PiiFilter`, `MaxOutputLength`, and `ShellCommandAllowlist` guardrails exist in `agentos-core::guardrails`.
- `MaxOrchestrator` provides deterministic Phase 2 command routing for `shell:`, `read file:`, and `http get:` prompts while preserving echo replies for ordinary text.
- `MaxOrchestrator` accepts a startup-time `ToolRegistry::specs()` catalog snapshot for future LLM-backed tool schema prompting without changing the semver-sensitive `RunContext` shape.
- `WebResearchSkill` exists as a planner skill backed by the migrated `workspace/skills/web-research/SKILL.md`; it emits normal `http` tool calls and summarizes only after the tool result returns through the loop.
- `HttpTool` supports HTTPS GETs through structured `curl` invocation, allowing the Hacker News reference workflow without adding a TLS client dependency.
- `agentos-cli` is wired with the reference tool registry and guardrails.
- CLI smoke coverage for echo, allowlisted shell, and blocked shell is automated in `agentos-cli` integration tests.
- Local verification passes: `cargo test -p agentos-core`, `cargo test -p agentos-cli`, `cargo check --workspace`, `cargo clippy --workspace -- -D warnings`, `cargo test --workspace`, `sh scripts/check-import-boundaries.sh`, `cargo semver-checks check-release -p agentos-interfaces --baseline-rev main`, and a live CLI Hacker News web research smoke with approved outbound network access.

Next work:

- Decide whether to replace the HTTPS `curl` fallback with a first-class TLS HTTP client in a later phase.
- Review whether the deterministic Phase 2 command syntax should remain developer-only or become documented CLI behavior.

Exit criterion target:

- "summarize the top story on Hacker News" works through the reference tool/skill path. Locally verified with approved outbound network access.
- A run that tries a disallowed shell command halts with `GuardrailTripped` naming `ShellCommandAllowlist` and the offending command. Locally verified with `shell: rm -rf /tmp/nope`.

## Phase 3 — Approve, Interruptions, and Resumable State

Status: started.

Completed:

- `agentos-core::approve::Policy` is now deny-by-default and evaluates explicit `allow`, `deny`, and `ask_user` decisions instead of acting as a no-op gate.
- A small YAML policy subset is implemented for Phase 3 policies with `default`, `rules`, `tool`, `action`, `decision`, `reason`, and scalar `args` matchers.
- The Phase 3 reference policy is available: shell asks for approval, file reads are allowed, and unmatched actions deny.
- The run loop routes non-terminal plans through the concrete policy engine before `Act`.
- `ask_user` on a tool call transitions to `Paused(RunState)` with a pending `Interruption`.
- Approved and rejected interruptions can be consumed through the resume path without adding a semver-breaking public field to `RunState`.
- A paused state can serialize to JSON, deserialize, approve, resume, execute the tool, and finish with the same trace shape as an equivalent unpaused run.
- `Policy::narrow` rejects child policies that widen `allow` or `ask_user` permissions beyond the parent policy.
- `agentos-cli` uses the Phase 3 reference policy and prompts for shell approval on the same terminal channel.
- `agentos-core::runner::PausedRun` stores paused `RunState` together with its channel and conversation ids for durable resume.
- File-backed paused-run helpers can save, load, and delete paused run records as JSON.
- `agentos-core::runner::resume_run` resumes approved interruptions through the same loop and session append path used by fresh runs.
- `agentos-cli resume <path> approve|reject` can load a saved paused run after process restart and continue or reject it.
- Approval prompts are represented as outbound `Envelope`s with `approval_prompt` metadata and are sent through the active channel.
- `agentos-cli` now reads approval decisions through `Channel::receive` instead of bypassing the channel with direct stderr/stdin handling.
- `agentos-core::gateway::GatewayService` now centralizes channel receive, runner dispatch, outbound reply send, approval prompt send, and resume dispatch for any `Channel` implementation.
- `agentos-cli` uses the gateway service for TUI, Telegram, cron smoke, and disk resume flows instead of open-coding runner/channel plumbing.

Remaining:

- Generalize interruptions beyond `ToolCall` so `ask_user` can pause handoffs and delegation without returning `ApprovalUnsupported`.
- Add tests for process-restart resume with durable session storage once Phase 4 persistence lands.
- Decide whether the default CLI paused-run path should remain `workspace/runs/cli-run-1.json` or move behind `workspace/agent.toml`.

Exit criterion target:

- A policy of "shell requires approval, read-only file access auto-allows, everything else denies" is enforceable. Partially verified locally with unit/integration coverage and CLI shell approval smoke tests.
- A run can pause and resume correctly after serializing `RunState`. Verified locally with JSON round-trip coverage, runner-level resume coverage, and CLI disk-resume smoke coverage; true 24-hour persistence waits on Phase 4 durable session storage.

## Phase 4 — Persistent Memory, Sessions, Crons, Second Channel

Status: started.

Completed:

- `agentos-core::memory::SqliteStore` exists as a SQLite-backed implementation of both `Memory` and `Session`.
- The SQLite schema stores memory records by namespace and session items by conversation/ordinal, preserving append order.
- SQLite memory records and session transcripts persist across process/store reopen in targeted tests.
- `agentos-cli` now uses `workspace/agentos.sqlite` for durable session storage by default, overrideable with `AGENTOS_SESSION_DB_PATH`.
- `agentos-core::tools::MemoryTool` exposes persistent memory through normal tool calls without changing public interfaces.
- `MaxOrchestrator` routes `remember:` and `recall:` prompts through the memory tool for a first user-visible recall path.
- `agentos-cli` registers the memory tool against the same SQLite store used for sessions.
- Targeted tests verify memory recall after SQLite reopen and CLI memory recall across separate processes.
- A Phase 3 pause/resume run now has SQLite-backed session reopen coverage.
- `agentos-core::crons` provides serializable interval schedules and cron tasks that enqueue ordinary `Envelope`s onto the bounded Gateway sender.
- Cron ingress is covered end-to-end through `Gateway -> runner -> output`, preserving the same trace shape and channel targeting as user-originated messages.
- `agentos-cli` includes a Telegram `Channel` implementation that maps Bot API updates to normal inbound envelopes and sends replies to the originating chat.
- `agentos-cli telegram-once` polls Telegram once and runs the same runner/approval path used by TUI without changing `agentos-core`.
- `agentos-cli telegram-cron-smoke` creates a due daily cron task targeting Telegram, sends it through `Gateway -> runner`, and posts the result through the Telegram channel.
- Live Telegram cron smoke passed with real bot credentials and emitted `trace: run=1, plan=1, llm=1`.
- `workspace/agent.toml` includes disabled-by-default Telegram channel wiring with environment-variable based secrets.

Remaining:

- Add the optional embedding index for semantic recall, or explicitly defer it after validating lexical/persistent recall.

Exit criterion target:

- The agent remembers facts across restarts. Locally verified with the reference SQLite memory tool and CLI `remember:` / `recall:` smoke coverage.
- A daily cron posts a summary to Telegram. Live credentialed smoke passed with `agentos-cli telegram-cron-smoke`.
- The same trace shape is emitted regardless of channel origin. Locally verified for TUI, cron ingress, and live Telegram cron smoke.

## Phase 5 — Sub-agents and MCPs with Real Isolation

Status: started.

Completed:

- `Policy::narrow` now rejects child policies with a more permissive default decision, closing the remaining default-policy widening path before sub-agent spawn.
- `agentos-core::subagents::SubAgentRegistry` can register named sub-agent definitions keyed by agent id and policy id.
- Delegation spawns the child run in a separate Tokio task and exchanges the child input/output through bounded `mpsc` channels.
- The run loop executes `Plan::Delegate` in `Act`, records a `delegate.<agent>` trace span, and appends the child result to the parent transcript as a normal observed item before replanning.
- Child policy derivation is enforced at spawn with `Policy::narrow(parent, child)`.
- Tests cover a parent delegating to a research child with HTTP-only access and a child shell attempt being denied at the child Approve layer.
- `ToolRegistry::register_mcp_server` adapts remote MCP tool specs into ordinary `Tool` implementations.
- MCP-backed tools execute through the existing tool path and produce the same `tool.<name>` trace span shape as local tools, with MCP server metadata attached to the result.
- `ToolRegistry::with_subprocess_isolation` can route tools whose `ToolSpec::requires_isolation` flag is true through an OS subprocess worker without changing the run loop.
- `agentos-tool-worker` provides the first subprocess-isolated tool backend for the reference `shell` tool using stdin/stdout JSON transport.
- The CLI can opt into subprocess isolation by setting `AGENTOS_TOOL_WORKER_PATH` to the worker binary path.
- `workspace/agent.toml` now has Phase 5 wiring for `isolation.worker_path_env` and a default HTTP-only `research-subagent` sub-agent.
- `agentos-cli` loads `workspace/agent.toml` or `AGENTOS_AGENT_CONFIG_PATH`, registers configured built-in sub-agents, and derives the parent delegate policy needed for child policy narrowing.
- `MaxOrchestrator` supports the developer command syntax `delegate <agent> <policy>: <prompt>` so configured sub-agents can be exercised through the normal runner path.
- `InterruptionAction` generalizes pending approvals beyond tool calls so delegate and handoff actions can serialize in paused `RunState`.
- `ask_user` on `Plan::Delegate` now pauses, round-trips through JSON, resumes through the normal approval path, and executes the child run after approval.
- `Plan::Handoff` now records a `handoff.<agent>` trace span, preserves optional payload metadata in the trace, switches `RunState.active_agent`, and replans under the new active agent.
- `ask_user` on `Plan::Handoff` now pauses, round-trips through JSON, resumes from the saved action, and then performs the active-agent switch.
- `SubAgentDefinition` can now own input, output, and tool guardrails for child runs, so delegated child tool calls are guarded inside the child runner rather than bypassing the parent safety layer.
- `workspace/agent.toml` supports `inherit_guardrails = true` for configured sub-agents; the CLI mirrors the reference input/output/tool guardrails into those child definitions.
- `workspace/agent.toml` now supports static `[[mcp_servers]]` and `[[mcp_tools]]` entries for deterministic local MCP smoke coverage.
- `agentos-cli` registers configured static MCP servers through the same `ToolRegistry::register_mcp_server` path used by real MCP clients, and allows configured MCP tools in the parent policy.
- `MaxOrchestrator` supports the developer command syntax `tool <name>: <input>` so configured MCP tools can be exercised through the normal runner path.
- `StdioMcpClient` can list and call tools exposed by a process-backed stdio endpoint through JSON-RPC-style `tools/list` and `tools/call` messages.
- `agentos-mcp-stdio-worker` provides deterministic local coverage for a live MCP subprocess transport, proving MCP-backed tools can be discovered from a running process rather than predeclared static config.
- `agentos-cli` now allows dynamically discovered MCP tool specs from registered MCP servers in the parent policy, so stdio-discovered tools can pass Approve without hardcoding static `[[mcp_tools]]` entries.
- `StdioMcpClient` now enforces a bounded worker timeout, kills timed-out workers, and returns a normal MCP failure instead of letting the runner hang.
- `workspace/agent.toml` supports `timeout_ms` on `[[mcp_servers]]`, allowing configured stdio MCP transports to set their process deadline without changing the run loop.
- `StdioMcpClient` now keeps a managed long-lived worker per stdio endpoint and reuses it across MCP calls, rather than spawning a fresh process for every request.
- `agentos-mcp-stdio-worker` now supports newline-delimited request handling so it can serve multiple MCP requests during one process lifetime.
- `agentos-llm::LlmOrchestrator` adapts the provider-neutral `Llm` trait into the existing `Orchestrator` trait by sending transcript messages as an `LlmRequest` and returning the provider response as `Plan::Reply`.

Remaining:

- Harden the stdio MCP transport toward the concrete MCP protocol/client selected for production use, including initialization/shutdown handshakes and stderr/log propagation.
- Extend subprocess isolation beyond the shell reference worker if additional `requires_isolation` tools are added.

Exit criterion target:

- A parent agent spawns a research sub-agent with read-only web access. Initial loop-level coverage is in place with an HTTP-only child.
- The parent has shell access, the child provably does not. Initial coverage proves a delegated child shell attempt is denied by the child Approve policy.
- MCP tools show up in traces with the same shape as local tools. Initial registry/loop coverage is in place with mock, static, and process-backed stdio MCP clients.
- Tools marked `requires_isolation` can execute through an OS subprocess. Initial coverage proves the reference `shell` tool runs through `agentos-tool-worker` while preserving the normal tool trace shape.
- Sub-agent and subprocess worker wiring can be declared in workspace config. Initial CLI smoke coverage proves config-defined sub-agents run through the normal TUI runner path.
- Delegate approvals can pause and resume from serialized state. Initial loop coverage proves an approved delegate continues from the saved action rather than replanning from the original user input.
- Handoff approvals can pause and resume from serialized state. Initial loop coverage proves an approved handoff switches the active agent and preserves normal trace shape.
- Child runs can carry inherited guardrails. Initial coverage proves a delegated child shell call is halted by the child `ShellCommandAllowlist`.
- MCP servers and tools can be declared in workspace config. Initial CLI smoke coverage proves configured static MCP-backed tools execute through the normal runner and tool registry path, and loop coverage proves live stdio-discovered MCP tools preserve the same trace shape.
- Stdio MCP calls are bounded. Initial coverage proves a slow process-backed MCP call times out and the worker process is killed.
- Stdio MCP workers have a managed lifecycle. Initial coverage proves two calls through the same stdio endpoint reuse the same worker process.

## Later Phases

Phase 6 starts after sub-agent policy narrowing, MCP-backed tool execution, subprocess isolation, and workspace-configured Phase 5 wiring meet the exit criterion.
