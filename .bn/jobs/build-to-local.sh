#!/bin/sh
# bn — build THIS checkout into a branch-scoped install dir, then link it in as the live bn.
#
# The command surface is `bn build build-to-local`. The heavy lifting stays in ./build.sh and
# install.sh; this job just picks the per-branch destination and the shared Cargo target dir.
set -eu

repo=$(git rev-parse --show-toplevel)
cd "$repo"

branch=$(git symbolic-ref --quiet --short HEAD 2>/dev/null || git rev-parse --short HEAD)
build_root="${XDG_DATA_HOME:-$HOME/.local/share}/bn/builds/branches/$branch"
target_root="${XDG_CACHE_HOME:-$HOME/.cache}/bn/target"

mkdir -p "$build_root" "$target_root"
export BN_BIN_DIR="$build_root"
export BN_INSTALL_TARGET_DIR="$target_root"

exec "$repo/build.sh"
