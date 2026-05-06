# AgentOS

AgentOS is a Rust-based local agent runtime with:

- an interactive TUI
- a persistent gateway for Telegram and Feishu
- pluggable LLM providers
- SQLite-backed session and memory storage

## Quick start

From a source checkout:

```sh
cp .env.example .env
scripts/install-agentos.sh --from-source
~/.local/bin/agentos tui
```

From a packaged release bundle:

```sh
tar -xzf agentos-v0.2.0-<platform>-<arch>.tar.gz
cd agentos-v0.2.0-<platform>-<arch>
scripts/install-agentos.sh
~/.local/bin/agentos tui
```

## Main scripts

- `scripts/install-agentos.sh`: install AgentOS from source or a release bundle
- `scripts/start-agentos.sh`: start the TUI, one-shot channel runs, or the persistent gateway
- `scripts/package-release.sh`: build and package a release archive
- `scripts/bootstrap-agentos.sh`: developer-oriented source checkout bootstrap

## Documentation

- [Install Guide](/Users/jiyuan/agents/codex/agentos/docs/INSTALL.md)
- [User Guide](/Users/jiyuan/agents/codex/agentos/docs/USER_GUIDE.md)
- [Release Notes](/Users/jiyuan/agents/codex/agentos/docs/RELEASE_NOTES.md)

## Release artifacts

Packaged releases are written to `dist/` by `scripts/package-release.sh`.
