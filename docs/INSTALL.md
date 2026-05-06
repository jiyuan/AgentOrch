# Install Guide

## Prerequisites

For source installs:

- `rustup`
- `cargo`
- `curl`
- `openssl` for Feishu long-connection support

For release-bundle installs:

- no Rust toolchain is required
- `curl` and `openssl` are still needed at runtime for Telegram and Feishu

## Install from source

```sh
cp .env.example .env
scripts/install-agentos.sh --from-source
```

This installs AgentOS into:

- binaries: `~/.local/bin`
- runtime home: `~/.local/share/agentos`

## Install from a release bundle

```sh
tar -xzf agentos-v0.2.0-<platform>-<arch>.tar.gz
cd agentos-v0.2.0-<platform>-<arch>
scripts/install-agentos.sh
```

## Configuration

After install, copy and edit:

```sh
cp ~/.local/share/agentos/.env.example ~/.local/share/agentos/.env
```

Minimum TUI setup:

```env
AGENTOS_LLM_PROVIDER=openai
AGENTOS_LLM_MODEL=gpt-5.4-mini
OPENAI_API_KEY=...
```

Telegram setup:

```env
AGENTOS_ENABLED_CHANNELS=telegram
AGENTOS_TELEGRAM_BOT_TOKEN=...
AGENTOS_TELEGRAM_CHAT_ID=...
```

Feishu setup:

```env
AGENTOS_ENABLED_CHANNELS=feishu
AGENTOS_FEISHU_APP_ID=...
AGENTOS_FEISHU_APP_SECRET=...
AGENTOS_FEISHU_ALLOWED_ID=...
```

## Verify install

```sh
~/.local/bin/agentos gateway-status
~/.local/bin/agentos tui
```
