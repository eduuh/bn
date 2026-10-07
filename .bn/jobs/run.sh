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

cargo build -p bn-cli -p bn-mcp
exec "${CARGO_TARGET_DIR:-$repo/target}/debug/bn" "$@"
