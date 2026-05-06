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
