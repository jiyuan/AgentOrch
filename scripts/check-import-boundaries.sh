#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

for manifest in "$root"/crates/agentos-core/Cargo.toml "$root"/crates/agentos-interfaces/Cargo.toml; do
  if grep -Eq '(\.\./\.\./workspace|\.\./\.\./extensions|workspace/|extensions/)' "$manifest"; then
    echo "import boundary violation in $manifest"
    exit 1
  fi
done

for source_root in "$root"/crates/agentos-core/src "$root"/crates/agentos-interfaces/src; do
  if grep -RInE 'workspace/|extensions/' "$source_root"; then
    echo "source-level import boundary violation in $source_root"
    exit 1
  fi
done

if command -v cargo >/dev/null 2>&1; then
  cargo tree -p agentos-core --manifest-path "$root/Cargo.toml" >/dev/null
  cargo tree -p agentos-interfaces --manifest-path "$root/Cargo.toml" >/dev/null
fi

echo "import boundaries ok"
