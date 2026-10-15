#!/bin/sh
# Build and run this worktree's CLI. Optional arguments are passed to bn.
set -eu

repo=$(git rev-parse --show-toplevel)
cd "$repo"

# shellcheck source=scripts/install-toolchain.sh
. "$repo/scripts/install-toolchain.sh"
bn_install_activate_cargo "$repo" || {
  echo "bn run: no usable Cargo toolchain found" >&2
  exit 1
}

target_dir="${CARGO_TARGET_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/bn/cargo-target/bn}"
head_sha=$(git -C "$repo" rev-parse HEAD 2>/dev/null || true)

mkdir -p "$target_dir"
GITHUB_SHA="${GITHUB_SHA:-$head_sha}" CARGO_TARGET_DIR="$target_dir" cargo build -p bn-cli -p bn-mcp
exec "$target_dir/debug/bn" "$@"
