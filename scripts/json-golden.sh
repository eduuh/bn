#!/usr/bin/env bash
# Golden-output capture for the `--json` envelope surface.
#
#   scripts/json-golden.sh capture <dir> [bn-binary]   record every command's envelope into <dir>
#   scripts/json-golden.sh compare <dir> [bn-binary]   re-run and diff against a recorded <dir>
#
# Why this exists: ~400 call sites build the `--json` envelope by hand, and the refactors that
# collapse them (a generic `Envelope::success`, the `json_ok` sweep, one shared payload wrapper)
# are only safe if the bytes on stdout do not move. Rust unit tests cover the types; this covers
# the actual CLI contract, which is what other tools parse.
#
# Every command below is READ-ONLY and network-free. The run is pinned to a throwaway
# BN_HOME/BN_STATE_DIR/notes root so it never reads or writes your real notes, and to a fixed
# BN_NOW/BN_HOSTNAME so repeated runs agree.
set -eu

MODE="${1:-}"
DIR="${2:-}"
BN="${3:-target/release/bn}"

case "$MODE" in
  capture|compare) ;;
  *) echo "usage: $0 capture|compare <dir> [bn-binary]" >&2; exit 2 ;;
esac
[ -n "$DIR" ] || { echo "usage: $0 $MODE <dir> [bn-binary]" >&2; exit 2; }
[ -x "$BN" ] || { echo "no bn binary at $BN (build it, or pass the path)" >&2; exit 2; }

# Read-only, offline, deterministic. Anything that fetches, spawns an agent, or writes state stays
# out — a golden file that depends on the network is a flake, not a gate.
COMMANDS='
todo --json
list --json
summary --json
doctor --json
next --json
attn --json
goal --json
cat --json
files --json
worktrees --json
task --json --all
daily --json
focus --json
queue --json
reviews --json
findings sources --json
links --json
skills --json
agents --json
harness --json
metrics --json
cost --json
sessions --json
machine --json
'

run_all() {
  out="$1"
  mkdir -p "$out"
  root=$(mktemp -d)
  trap 'rm -rf "$root"' RETURN
  mkdir -p "$root/notes" "$root/state" "$root/home"

  n=0
  echo "$COMMANDS" | while IFS= read -r cmd; do
    [ -n "$cmd" ] || continue
    n=$((n + 1))
    # Slug: "findings sources --json" -> "findings-sources"
    slug=$(printf '%s' "$cmd" | sed 's/--[a-z-]*//g; s/[^a-zA-Z0-9]\+/-/g; s/-\+$//; s/^-\+//')
    # shellcheck disable=SC2086  # $cmd is an intentional argument list
    env -i \
      PATH="$PATH" \
      HOME="$root/home" \
      BN_HOME="$root/home/.bn" \
      BN_STATE_DIR="$root/state" \
      BN_NOTES_DIR="$root/notes" \
      BN_HOSTNAME="golden-host" \
      BN_NOW="1780000000" \
      "$PWD/$BN" $cmd >"$out/$slug.json" 2>"$out/$slug.err" || true
  done
}

case "$MODE" in
  capture)
    run_all "$DIR"
    echo "captured $(find "$DIR" -name '*.json' | wc -l) envelopes into $DIR"
    ;;
  compare)
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    run_all "$tmp"
    if diff -ru "$DIR" "$tmp"; then
      echo "OK — every --json envelope is byte-identical to $DIR"
    else
      echo "CHANGED — the --json surface moved; see the diff above" >&2
      exit 1
    fi
    ;;
esac
