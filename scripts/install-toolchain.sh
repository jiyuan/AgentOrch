#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

rust_toolchain="${AGENTOS_RUST_TOOLCHAIN:-stable}"
install_semver_checks="${AGENTOS_INSTALL_SEMVER_CHECKS:-1}"

cargo_for_toolchain() {
  rustup run "$rust_toolchain" cargo "$@"
}

usage() {
  cat <<'USAGE'
Usage: scripts/install-toolchain.sh [--skip-semver-checks]

Configures the local AgentOS Rust toolchain.

Environment:
  AGENTOS_RUST_TOOLCHAIN          Rust toolchain to install/use. Default: stable
  AGENTOS_INSTALL_SEMVER_CHECKS  Install cargo-semver-checks when set to 1. Default: 1
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-semver-checks)
      install_semver_checks=0
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if ! command -v rustup >/dev/null 2>&1; then
  cat >&2 <<'ERROR'
rustup is required but was not found.
Install it from https://rustup.rs, then rerun this script.
ERROR
  exit 1
fi

echo "Installing Rust toolchain: ${rust_toolchain}"
rustup toolchain install "$rust_toolchain"
rustup component add clippy rustfmt --toolchain "$rust_toolchain"

if [[ "$install_semver_checks" == "1" ]]; then
  if cargo_for_toolchain semver-checks --version >/dev/null 2>&1; then
    echo "cargo-semver-checks is already installed"
  else
    echo "Installing cargo-semver-checks"
    cargo_for_toolchain install cargo-semver-checks --locked
  fi
else
  echo "Skipping cargo-semver-checks installation"
fi

echo "Checking workspace toolchain"
cargo_for_toolchain fmt --all --check --manifest-path "$root/Cargo.toml"
cargo_for_toolchain check --workspace --manifest-path "$root/Cargo.toml"

echo "AgentOS toolchain is ready"
