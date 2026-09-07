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
# Env knobs:
#   BN_INSTALL_TARGET_DIR                Cargo target dir     (default: target/bn-install)
#   BN_INSTALL_PROFILE                   Cargo profile        (default: local — see Cargo.toml;
#                                        `release` for the fat-LTO binary the published release ships)
#   BN_SKIP_UI=1                         skip the bn-web UI build (embed whatever is in ui/dist)
#   BN_REBUILD_UI=1                      force an npm reinstall AND rebuild of the UI
#   BN_INSTALL_KILL_CARGO_LOCK_HOLDER=1  kill a stale Cargo process holding the target dir's lock
set -euo pipefail
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=scripts/install-toolchain.sh
source "$repo/scripts/install-toolchain.sh"

# Exported so install.sh --from-build reads the binaries out of the same dir this build wrote to.
BN_INSTALL_TARGET_DIR="${BN_INSTALL_TARGET_DIR:-$repo/target/bn-install}"
export BN_INSTALL_TARGET_DIR

# sha256 of stdin via whichever tool the platform ships; cksum is a weak last resort, but it still
# changes when the input does, which is all these comparisons need.
fingerprint_stdin() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 | cut -d' ' -f1
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum | cut -d' ' -f1
  else
    cksum | cut -d' ' -f1
  fi
}

# Fingerprint of the file that pins the UI's dependency tree — the lockfile when there is one, else
# package.json. Stamped into node_modules after a successful install so the next run can tell a
# stale tree from a current one.
ui_deps_fingerprint() {
  fingerprint_stdin <"$1"
}

# Fingerprint of everything the UI build reads: every file under ui/ except the installed
# dependency tree and the build output. Paths are part of the hash, so a rename or deletion counts
# as a change too. Stamped into dist/ after a successful build so the next install can skip the
# ~45s `tsc && vite build` when nothing it consumes has moved — and, because dist/ is then left
# untouched, bn-web's build.rs (`rerun-if-changed=ui/dist`) does not recompile bn-web either.
ui_src_fingerprint() {
  (
    cd "$1" &&
      find . -path ./node_modules -prune -o -path ./dist -prune -o -type f -print |
      LC_ALL=C sort |
      while IFS= read -r f; do printf '%s\n' "$f"; cat "$f"; done
  ) | fingerprint_stdin
}

# Build the bn-web dashboard UI (crates/bn-web/ui → dist) so build.rs embeds the real
# single-page app instead of the "UI not built" placeholder. Node-optional and best-effort:
# with no npm we leave the placeholder and the server (JSON API + MCP + dashboard shell) still runs.
# Rebuilds the UI whenever its sources changed since dist/ was last built, so a from-source build
# always embeds the current dashboard — build.rs re-embeds via `cargo:rerun-if-changed` on ui/dist,
# so the recompiled bn-web picks the change up. Set BN_SKIP_UI=1 to skip it unconditionally.
#
# Dependencies are restored whenever the installed tree does not match the lockfile — missing, or
# stale because a pull moved the lockfile under an existing node_modules. That staleness used to be
# invisible: the old check installed only when node_modules was *absent*, so pulling a commit that
# added a dependency left the previous tree in place and `npm run build` failed on the package that
# was in package.json but never installed. Comparing a stamped fingerprint keeps the repeat build
# fast (no reinstall when nothing moved) without ever building against a stale tree.
# BN_REBUILD_UI=1 forces the reinstall regardless. Never aborts the install — a UI hiccup must not
# block the Rust binary.
build_web_ui() {
  local ui="$repo/crates/bn-web/ui" fresh_deps=1
  [ -f "$ui/package.json" ] || return 0
  if [ "${BN_SKIP_UI:-0}" = "1" ]; then
    echo "bn-web UI build skipped (BN_SKIP_UI=1) — the binary embeds whatever is already in $ui/dist."
    return 0
  fi
  if ! command -v npm >/dev/null 2>&1; then
    echo "note: npm not found — bn-web will serve the 'UI not built' placeholder page." >&2
    echo "      Install Node.js/npm and re-run to embed the dashboard UI." >&2
    return 0
  fi
  local pin="$ui/package.json" stamp="$ui/node_modules/.bn-deps-stamp" want=""
  [ -f "$ui/package-lock.json" ] && pin="$ui/package-lock.json"
  want=$(ui_deps_fingerprint "$pin")
  if [ -d "$ui/node_modules" ] && [ "${BN_REBUILD_UI:-0}" != "1" ] &&
    [ -f "$stamp" ] && [ "$(cat "$stamp")" = "$want" ]; then
    fresh_deps=0
  fi
  local src_stamp="$ui/dist/.bn-src-stamp" src_want=""
  src_want=$(ui_src_fingerprint "$ui")
  if [ "$fresh_deps" = "0" ] && [ -f "$ui/dist/index.html" ] &&
    [ -f "$src_stamp" ] && [ "$(cat "$src_stamp")" = "$src_want" ]; then
    echo "bn-web dashboard UI is up to date ($ui/dist matches its sources) — skipping the UI build."
    return 0
  fi
  echo "Building the bn-web dashboard UI (npm + vite) so the binary embeds it…"
  (
    cd "$ui"
    if [ "$fresh_deps" = "1" ]; then
      if [ -f package-lock.json ]; then npm ci; else npm install; fi
      # After the install, not before: `npm ci` wipes node_modules (stamp included), and stamping
      # only on success means a failed install stays stale and retries next run.
      printf '%s\n' "$want" >"$stamp"
    fi
    npm run build
    # Stamp only after a successful build, so a failed one is retried next run.
    printf '%s\n' "$src_want" >"$src_stamp"
  ) || {
    echo "warning: bn-web UI build failed — installing with the placeholder page." >&2
    echo "         Fix Node/npm and re-run to embed the dashboard UI." >&2
    return 0
  }
  echo "Built the bn-web dashboard UI ($ui/dist)."
}

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
  # bn-web bakes its dashboard UI in at compile time (build.rs embeds crates/bn-web/ui/dist). Build
  # that UI first when node is available so the from-source binary serves the real dashboard, not
  # the "UI not built" placeholder; build_web_ui is node-optional (no npm → placeholder, server
  # still runs). CI builds the UI the same way before the release binary.
  build_web_ui
  # bn-mcp is built alongside: `register_mcp` points every MCP client at the stdio launcher
  # `bn mcp serve`, which execs the sibling `bn-mcp`. Omitting it left that launcher dead
  # ("could not exec bn-mcp") and silently cost Claude Code / Copilot every bn tool and
  # session-safe skill prompt — a failure only visible inside the client, never from `bn` itself.
  # This build uses its own target dir so self-update/nightly-release/dev builds do not block each
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
  ( cd "$repo" && GITHUB_SHA="${GITHUB_SHA:-$head_sha}" CARGO_TARGET_DIR="$target_dir" cargo build --profile "$BN_INSTALL_PROFILE" -p bn-cli -p bn-web -p bn-mcp )
  echo "Built bn + bn-web + bn-mcp in $target_dir/$BN_INSTALL_PROFILE."
}

build_from_source
# Hand the freshly built binaries to install.sh, which places them and wires every layer. Flags
# ride along untouched, so `./build.sh --core` means what `./install.sh --core` means.
exec "$repo/install.sh" --from-build "$@"