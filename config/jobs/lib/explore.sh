#!/usr/bin/env bash
# explore — the discovery arm of autonomous mode.
#
# Everything else in bn finds work by *reading*: `sentinel` patrols static debt, `scout` watches
# upstreams, the review gates read diffs. Nothing ever starts the product. This job does: it
# dispatches ONE scout per repo per pass, seeded with the `explore` skill, which boots the app,
# drives it like a user, reduces what breaks to a deterministic reproduction, and files only what
# reproduces (`bn issue file --kind bug --label autonomous-repro`).
#
# It files; it does not fix. The existing fix arm takes it from there — `bn task pull` drains the
# issue reservoir, `task enhance` makes each SMART, a supervisor dispatches a fixer, `bn gate` and
# `bn critic` prove the fix, and a PR lands in the review queue. Merging stays a human act.
#
# This is the shared driver, and it lives under `lib/` because of it. Each repo gets a thin wrapper
# that sets the lane's settings and execs it — `explore-bn.sh`, `explore-wira360.sh` — and the agent
# manifest of the same name resolves straight to that wrapper. Run a lane, never the driver:
#   bn run explore-bn
#
# The settings live in the wrapper rather than on a manifest `run:` line. A lane is fired by a timer
# through the scheduler, never from a shell, so the wrapper is the one place its settings are always
# set and reviewable in git: a `run: "BN_EXPLORE_REPO=bn bn run explore"` reached this driver with
# nothing set and exited 2.
#
# Sitting in `jobs/` was the second half of that bug. Job names resolve flat — `<jobs>/<name>.sh` —
# so a driver parked there answers to `bn run explore`, exits 2 for the missing repo, and books the
# failure against a cron nobody declared, where it reads as a live outage at the top of `bn next`.
# `lib/` is not scanned, so the name simply stops resolving. Unlike its neighbours here it is exec'd
# by path, not sourced.
#
# Env knobs:
#   BN_EXPLORE_REPO      REQUIRED — repo to explore (a bare repo under the bare root)
#   BN_EXPLORE_FOCUS     optional focus area folded into the assignment (default: whole app)
#   BN_EXPLORE_MAX       cap on issues one pass may file        (default: 5)
#   BN_EXPLORE_BACKLOG   skip while this many open autonomous-repro issues are unfixed (default: 10)
#   BN_EXPLORE_AGENT     harness for the scout                  (default: the configured one)
#   BN_EXPLORE_YOLO      1 to launch without approval prompts   (default: 1 — unattended by design)
#   BN_EXPLORE_OFF       1 to disable; also ~/.bn/explore.off    (kill switch)
#   BN_EXPLORE_DRYRUN    1 to print the dispatch instead of running it
#
# Graceful degradation: every guard that cannot be evaluated skips the run rather than guessing.
# Starting a second explorer on a repo is worse than missing a night.
set -u

BN="${BN:-bn}"
REPO="${BN_EXPLORE_REPO:-}"
MAX="${BN_EXPLORE_MAX:-5}"
BACKLOG_CAP="${BN_EXPLORE_BACKLOG:-10}"
LABEL="autonomous-repro"
export PATH="$HOME/.local/bin:$HOME/.bin:$PATH"

log() { printf '  %s\n' "$*" >&2; }

[ -n "$REPO" ] || { log "explore: BN_EXPLORE_REPO is unset — nothing to explore"; exit 2; }

# --- kill switches ------------------------------------------------------------------------
# One global switch and one per repo, both as files so they survive a reboot and can be flipped
# without editing a manifest: `touch ~/.bn/explore.off`.
[ "${BN_EXPLORE_OFF:-0}" = "1" ] && { log "explore: disabled by BN_EXPLORE_OFF"; exit 0; }
[ -f "$HOME/.bn/explore.off" ] && { log "explore: disabled by ~/.bn/explore.off"; exit 0; }
[ -f "$HOME/.bn/explore.$REPO.off" ] && { log "explore: $REPO disabled by ~/.bn/explore.$REPO.off"; exit 0; }

command -v jq >/dev/null 2>&1 || { log "explore: jq not found — skipping (cannot check for a live explorer)"; exit 0; }

# --- don't stack explorers ----------------------------------------------------------------
# One per repo. A second explorer would drive the same app against the same local ports and
# file the same bugs under near-identical titles, which the dedup key would NOT catch.
#
# Match `explore[-/]`, both separators. The dispatched branch is `explore/<stamp>`, but the fleet
# reports it as `explore-<stamp>` — tmux window names cannot hold a slash, so the name is
# normalized on the way out. Testing only for the slash made this guard dead code: it never
# matched a single live explorer, and the check silently passed every time.
# `done`/`gone` are excluded because they are not driving anything. A finished explorer's harness
# session stays resident long after its pass ends, so counting it would let one completed run block
# every future one — the lane would fire once and then skip for as long as the session lingered.
live="$("$BN" fleet --json 2>/dev/null \
  | jq -r --arg repo "$REPO" '
      [ .data.agents[]?
        | select(.repo == $repo)
        | select(.branch | test("^explore[-/]"))
        | select(.agent_alive == true)
        | select(.state != "done" and .state != "gone")
      ] | length' 2>/dev/null)" || live=""

if [ -z "$live" ]; then
  log "explore: could not read the fleet — skipping rather than risk a second explorer"
  exit 0
fi
if [ "$live" -gt 0 ]; then
  log "explore: $REPO already has $live live explorer(s) — leaving it alone"
  exit 0
fi

# --- backpressure -------------------------------------------------------------------------
# The explorer is much cheaper than the fixers it feeds, so unchecked it buries the fix arm in
# reproduced bugs nobody is working yet. Stop filing while the backlog it already created is
# undrained — a growing pile of open autonomous-repro issues means the fix arm, not discovery,
# is the constraint.
#
# `gh` resolves the remote from the working directory, and a cron's cwd is arbitrary, so the count
# is taken from inside the repo's own main worktree. A repo whose worktree isn't where we expect
# skips the check rather than the run — an unenforced cap is a smaller failure than never exploring.
repo_main="${BN_WORKTREE_DIR:-$HOME/projects/worktree}/$REPO/main"
[ -d "$repo_main" ] || {
  log "explore: no worktree at $repo_main — skipping unavailable repo"
  exit 0
}
if command -v gh >/dev/null 2>&1; then
  open_backlog="$(cd "$repo_main" && gh issue list --label "$LABEL" --state open --limit 100 \
    --json number 2>/dev/null | jq 'length' 2>/dev/null)" || open_backlog=""
  if [ -n "$open_backlog" ] && [ "$open_backlog" -ge "$BACKLOG_CAP" ]; then
    log "explore: $REPO has $open_backlog open $LABEL issues (cap $BACKLOG_CAP) — the fix arm is the constraint, not discovery"
    exit 0
  fi
else
  log "explore: skipping the backlog check (no gh)"
fi

# --- the assignment -----------------------------------------------------------------------
focus="${BN_EXPLORE_FOCUS:-}"
if [ -n "$focus" ]; then
  assignment="Explore the $REPO app, focusing on: $focus. File at most $MAX reproduced issues."
else
  assignment="Explore the $REPO app end to end — every surface it claims to have, then the unscripted paths a real user takes. File at most $MAX reproduced issues."
fi

# An extra label routes a repo's findings somewhere other than the default fix lane — a
# product-facing repo files `needs-triage` so a human decides before any fixer picks it up.
if [ -n "${BN_EXPLORE_LABEL:-}" ]; then
  assignment="$assignment Label every issue you file with --label $BN_EXPLORE_LABEL in addition to $LABEL."
fi

branch="explore/$(date +%Y%m%d-%H%M)"

set -- --repo "$REPO" --scout --skill explore --branch "$branch" \
       --name "explore $REPO" --fresh --agent
[ -n "${BN_EXPLORE_AGENT:-}" ] && set -- "$@" "$BN_EXPLORE_AGENT"
[ "${BN_EXPLORE_YOLO:-1}" = "1" ] && set -- "$@" --yolo

# `--` before the assignment is load-bearing: `--agent` takes an OPTIONAL value, so an unpinned
# harness with no flag after it would swallow the assignment text as the harness name and dispatch
# an agent called "Explore the … app".
set -- "$@" --

if [ "${BN_EXPLORE_DRYRUN:-0}" = "1" ]; then
  log "DRYRUN: $BN dispatch $* \"$assignment\""
  exit 0
fi

if "$BN" dispatch "$@" "$assignment"; then
  log "explore: dispatched a scout on $REPO/$branch (cap $MAX)"
  exit 0
fi

log "explore: dispatch failed for $REPO"
exit 1
