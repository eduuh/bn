#!/usr/bin/env bash
# Build bn from this checkout, then install what it produced.
#
# ./install.sh is the fast path and never compiles: it downloads main's published release
# binaries. Use this script when the binaries you want are not in that release — an unmerged
# branch, a local patch, or a platform the release does not publish for.
#
# Layers and flags belong to install.sh: every argument is passed straight through, and once the
# build succeeds this script re-enters install.sh with --from-build so the anchors, tmux layer,
# cron and MCP registration all happen there, in one place.
#
# Dotfile linking (config/stow, via scripts/install-stow.sh) also happens in install.sh, so
# `./build.sh --force-home` rebuilds and adopts the tracked configs and skills in one go.
#
# Env knobs:
#   BN_INSTALL_TARGET_DIR                Cargo target dir     (default: target/bn-install)
#   BN_INSTALL_PROFILE                   Cargo profile        (default: local — see Cargo.toml;
#                                        `release` for the fat-LTO binary the published release ships)
#   BN_INSTALL_KILL_CARGO_LOCK_HOLDER=1  kill a stale Cargo process holding the target dir's lock
set -euo pipefail
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=scripts/install-toolchain.sh
source "$repo/scripts/install-toolchain.sh"

# Exported so install.sh --from-build reads the binaries out of the same dir this build wrote to.
BN_INSTALL_TARGET_DIR="${BN_INSTALL_TARGET_DIR:-$repo/target/bn-install}"
export BN_INSTALL_TARGET_DIR

kill_cargo_lock_holders() {
  local lock="$1" pids="" pid
  [ "${BN_INSTALL_KILL_CARGO_LOCK_HOLDER:-0}" = "1" ] || return 0
  [ -e "$lock" ] || return 0
  if command -v lsof >/dev/null 2>&1; then
    pids=$(lsof -t "$lock" 2>/dev/null | sort -u || true)
  elif command -v fuser >/dev/null 2>&1; then
    pids=$(fuser "$lock" 2>/dev/null | tr ' ' '\n' | sort -u || true)
  fi
  [ -n "$pids" ] || return 0
  echo "install: terminating Cargo process(es) holding $lock: $(echo "$pids" | tr '\n' ' ')" >&2
  for pid in $pids; do
    [ "$pid" != "$$" ] && kill "$pid" 2>/dev/null || true
  done
  sleep 2
  for pid in $pids; do
    [ "$pid" != "$$" ] && kill -0 "$pid" 2>/dev/null && kill -9 "$pid" 2>/dev/null || true
  done
}

build_from_source() {
  bn_install_activate_cargo "$repo" || {
    echo "error: no usable Cargo toolchain was found." >&2
    echo "       Install Rust with rustup, repair its proxies, or set CARGO_HOME/RUSTUP_HOME." >&2
    exit 1
  }
  local target_dir="${BN_INSTALL_TARGET_DIR:-$repo/target/bn-install}"
  # Exported so install.sh --from-build reads the binaries out of the profile dir this build wrote
  # to. `local` (Cargo.toml) is release-optimised without fat LTO / one codegen unit, which is what
  # makes an install after every commit affordable; `release` is the tight published binary.
  BN_INSTALL_PROFILE="${BN_INSTALL_PROFILE:-local}"
  export BN_INSTALL_PROFILE
  echo "Building Rust bn from source with $BN_INSTALL_CARGO_DIR/cargo (profile $BN_INSTALL_PROFILE)…"
  # bn-mcp is built alongside: `register_mcp` points every MCP client at the stdio launcher
  # `bn mcp serve`, which execs the sibling `bn-mcp`. Omitting it left that launcher dead
  # ("could not exec bn-mcp") and silently cost Claude Code / Copilot every bn tool and
  # session-safe skill prompt — a failure only visible inside the client, never from `bn` itself.
  # This build uses its own target dir so self-update and dev builds do not block each
  # other on Cargo's shared `target/.cargo-lock`. If that target is wedged by a dead prior build,
  # opt in to clearing it with BN_INSTALL_KILL_CARGO_LOCK_HOLDER=1.
  mkdir -p "$target_dir"
  kill_cargo_lock_holders "$target_dir/.cargo-lock"
  kill_cargo_lock_holders "$target_dir/$BN_INSTALL_PROFILE/.cargo-lock"
  # Pin the provenance bn-cli's build.rs stamps into `bn version` to THIS checkout's HEAD. The
  # build script otherwise asks git itself and re-runs only when the HEAD paths it recorded last
  # time move — and with a target dir shared between worktrees those are another checkout's
  # paths, so a build from a pool slot reported main's sha (observed 2026-09-07). GITHUB_SHA is
  # the override build.rs already honours (CI sets it) and tracks with rerun-if-env-changed, so a
  # different HEAD always re-runs it.
  local head_sha
  head_sha=$(git -C "$repo" rev-parse HEAD 2>/dev/null || true)
  ( cd "$repo" && GITHUB_SHA="${GITHUB_SHA:-$head_sha}" CARGO_TARGET_DIR="$target_dir" cargo build --profile "$BN_INSTALL_PROFILE" -p bn-cli -p bn-mcp )
  echo "Built bn + bn-mcp in $target_dir/$BN_INSTALL_PROFILE."
}

build_from_source
# Hand the freshly built binaries to install.sh, which places them and wires every layer. Flags
# ride along untouched, so `./build.sh --core` means what `./install.sh --core` means.
exec "$repo/install.sh" --from-build "$@"