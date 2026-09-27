#!/bin/sh
# bn — build THIS checkout into the prod install dir, then link it in as the live bn.
#
# The command surface is `bn build publish-prod`. The heavy lifting stays in ./build.sh and
# install.sh; this job just picks the prod destination and the shared Cargo target dir.
set -eu

repo=$(git rev-parse --show-toplevel)
cd "$repo"

build_root="${XDG_DATA_HOME:-$HOME/.local/share}/bn/builds/prod"
target_root="${XDG_CACHE_HOME:-$HOME/.cache}/bn/target"

mkdir -p "$build_root" "$target_root"
export BN_BIN_DIR="$build_root"
export BN_INSTALL_TARGET_DIR="$target_root"

exec "$repo/build.sh"
