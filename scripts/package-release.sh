#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dist_dir="${DIST_DIR:-$root/dist}"
rust_toolchain="${AGENTOS_RUST_TOOLCHAIN:-stable}"

version="$(awk -F'"' '/^version = / { print $2; exit }' "$root/Cargo.toml")"
platform="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"

case "$platform" in
  darwin) platform="darwin" ;;
  linux) platform="linux" ;;
esac

case "$arch" in
  arm64) arch="arm64" ;;
  aarch64) arch="arm64" ;;
  x86_64) arch="x86_64" ;;
esac

bundle_name="agentos-v${version}-${platform}-${arch}"
stage_dir="$dist_dir/$bundle_name"
archive_path="$dist_dir/$bundle_name.tar.gz"
checksum_path="$archive_path.sha256"

mkdir -p "$dist_dir"
rm -rf "$stage_dir" "$archive_path" "$checksum_path"

rustup run "$rust_toolchain" cargo build \
  --release \
  --manifest-path "$root/Cargo.toml" \
  -p agentos-cli \
  -p agentos-core \
  --bins

install -d "$stage_dir/bin" "$stage_dir/scripts" "$stage_dir/docs" "$stage_dir/workspace"

for binary in agentos-cli agentos-gateway agentos-tool-worker agentos-mcp-stdio-worker; do
  install -m 755 "$root/target/release/$binary" "$stage_dir/bin/$binary"
done

install -m 755 "$root/scripts/install-agentos.sh" "$stage_dir/scripts/install-agentos.sh"
install -m 755 "$root/scripts/start-agentos.sh" "$stage_dir/scripts/start-agentos.sh"
install -m 644 "$root/.env.example" "$stage_dir/.env.example"
install -m 644 "$root/workspace/agent.toml" "$stage_dir/workspace/agent.toml"
install -m 644 "$root/README.md" "$stage_dir/README.md"
install -m 644 "$root/LICENSE" "$stage_dir/LICENSE"
install -m 644 "$root/docs/INSTALL.md" "$stage_dir/docs/INSTALL.md"
install -m 644 "$root/docs/USER_GUIDE.md" "$stage_dir/docs/USER_GUIDE.md"
install -m 644 "$root/docs/RELEASE_NOTES.md" "$stage_dir/docs/RELEASE_NOTES.md"
printf '%s\n' "$version" >"$stage_dir/VERSION"

LC_ALL=C LANG=C tar -C "$dist_dir" -czf "$archive_path" "$bundle_name"

write_checksum() {
  local target="$1"
  if command -v shasum >/dev/null 2>&1; then
    LC_ALL=C LANG=C shasum -a 256 "$target" >"$checksum_path" && return 0
    rm -f "$checksum_path"
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    LC_ALL=C LANG=C sha256sum "$target" >"$checksum_path" && return 0
    rm -f "$checksum_path"
  fi
  return 1
}

if ! write_checksum "$archive_path"; then
  echo "warning: unable to generate release checksum" >&2
fi

echo "Release bundle created:"
echo "  $archive_path"
if [[ -f "$checksum_path" ]]; then
  echo "  $checksum_path"
fi
