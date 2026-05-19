# Release Notes

## v0.5.0

Released 2026-05-19.

- Reworked the sub-agent and skill mechanism.
- Added new bundled workspace skills.
- Documentation refresh across the `docs/` set.
- Test fixes and `.gitignore` cleanup.

## v0.4.0

- Reworked skill creation: rebuilt `skill-creator` planner logic and the
  `agentos skill create` flow, including bundle completeness handling.
- Cron tooling: refactored `cron-create` and fixed cron scheduling issues.
- Sub-agent improvements: tool calling for sub-agents, output-length limits,
  per-task session isolation, and attachment propagation from the main agent.
- Multimodal support: attachment handling for Telegram and Feishu, and
  multimodal requests for the OpenAI and Anthropic providers.
- Fixed `MaxOrchestrator` not invoking tools.
- Telegram/Feishu message chunking limit fixes.
- Feishu reliability: proxy fix and openssl stderr capture on websocket
  failures.

## v0.3.0

- Unified slash commands across the TUI, Telegram, and Feishu surfaces.
- Added TUI list commands (`/skills`, `/crons`, `/tools`, `/memory`, `/usage`).
- Phase 1–5 milestones completed (typed loop, approval, tools, memory,
  channels).
- Bash 3.2 CLI argument-expansion fix.

## v0.2.0

Baseline packaged AgentOS workspace:

- interactive TUI support
- persistent Telegram and Feishu gateway support
- pluggable LLM providers: OpenAI, Anthropic, DeepSeek, Ollama, and a
  `builtin.echo` offline fallback
- a typed run loop with concrete approval policy and content guardrails
- SQLite-backed sessions and scoped long-term memory
- configured sub-agents and sub-orchestrator templates
- static and stdio MCP-backed tools, with subprocess isolation
- workspace skills in the Anthropic `SKILL.md` format
- user-facing install and startup scripts
- source and bundle installation paths
- release packaging automation

Release contents:

- `agentos-cli`
- `agentos-gateway`
- `agentos-tool-worker`
- `agentos-mcp-stdio-worker`
- default `workspace/agent.toml`
- `.env.example`
- install/startup scripts
- user documentation
