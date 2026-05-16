# User Guide

## Installed commands

After installation, the main entrypoint is:

```sh
~/.local/bin/agentos
```

## Modes

### TUI

```sh
agentos tui
```

Starts the interactive terminal interface.

Useful commands inside the TUI:

- `/clear`
- `/orchestrator status`
- `/orchestrator max`
- `/orchestrator min`
- `/model status`
- `/model reset`

### Gateway lifecycle

```sh
agentos gateway-start
agentos gateway-restart
agentos gateway-stop
agentos gateway-status
```

The gateway uses:

- config: `~/.local/share/agentos/workspace/agent.toml`
- env file: `~/.local/share/agentos/.env`
- session db: `~/.local/share/agentos/workspace/agentos.sqlite`
- log: `~/.local/share/agentos/logs/agentos-gateway.log`

To inspect the loaded runtime configuration, run:

```sh
agentos-gateway config --config workspace/agent.toml
```

Gateway persistent channel selection uses this precedence:

1. `AGENTOS_ENABLED_CHANNELS=telegram,feishu` overrides workspace channel enablement.
2. Without that override, `[channels.telegram].enabled` and `[channels.feishu].enabled` in `workspace/agent.toml` control persistent channels.
3. `channels.tui` is for the interactive TUI and is never started by the persistent gateway.

The effective config keys are:

- `[agent].max_turns`, `[policy].default`, `[channels.*].enabled`, and `[channels.*].mode`.
- `[resources.skills].enabled`, `[resources.tools].enabled`, `[resources.mcp].enabled`, and `[resources.llm].enabled`.
- Workspace file discovery from `subagents/*.toml` and `suborchs/*.toml` through the same loader used by the runtime.
- Runtime path injection for workspace root, skills, crons, traces, sessions, and channel attachments is documented in `docs/ARCHITECTURE.md`.

### One-shot channel runs

```sh
agentos telegram-once
agentos feishu-once
```

These process a single inbound event and exit.

## Logs and state

- gateway log: `~/.local/share/agentos/logs/agentos-gateway.log`
- session database: `~/.local/share/agentos/workspace/agentos.sqlite`
- paused runs: `~/.local/share/agentos/workspace/runs/`

## Packaging a release

From a source checkout:

```sh
scripts/package-release.sh
```

Artifacts are written to `dist/`.
