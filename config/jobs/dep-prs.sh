#!/usr/bin/env bash
# dep-prs -- land the dependency-update PRs (Dependabot, Renovate) across every repo you own.
#
# Fired daily by the scheduler (agents/dep-prs.md), pinned to a worker machine with
# `[jobs.only_on] dep-prs = ["<machine>"]`. For each repo with open dependency PRs it dispatches
# one agent seeded with the `dep-prs` skill, which closes superseded PRs, merges green
# patch/minor bumps, fixes red ones and leaves majors to a human. Finding no PRs costs one GitHub
# search and no agent.
#
# The agent reports back with `bn agent msg` to BN_DEP_REPORT_TO (a repo:branch, on any mesh
# machine), so a coordinator on the laptop hears what landed and what is stuck.
#
# Env knobs (or the same names in ~/.config/bn/dep-prs.env, sourced first):
#   BN_DEP_OWNER       GitHub owner to sweep           (default: the account gh is signed in as)
#   BN_DEP_REPORT_TO   repo:branch to report to        (default: nobody)
#   BN_DEP_MAX         agents live at once, all repos  (default: 1; a build per agent)
#   BN_DEP_AGENT       harness                          (default: the configured one)
#   BN_DEP_REPOS       space-separated repo names; only these (default: all with dependency PRs)
#   BN_DEP_DRYRUN      1 to print the dispatches instead of running them
# Kill switch: ~/.bn/dep-prs.off (all repos) or ~/.bn/dep-prs.<repo>.off.
set -u

BN="${BN:-bn}"
export PATH="$HOME/.bin:$HOME/.bn/bin:$HOME/.local/bin:$PATH"
[ -f "$HOME/.config/bn/dep-prs.env" ] && . "$HOME/.config/bn/dep-prs.env"
MAX="${BN_DEP_MAX:-1}"
ROOT="${BN_WORKTREE_DIR:-$HOME/projects/worktree}"

log() { printf '  %s\n' "$*" >&2; }

[ -f "$HOME/.bn/dep-prs.off" ] && { log "dep-prs: disabled by ~/.bn/dep-prs.off"; exit 0; }
command -v gh >/dev/null 2>&1 && command -v jq >/dev/null 2>&1 || { log "dep-prs: needs gh and jq"; exit 0; }
OWNER="${BN_DEP_OWNER:-$(gh api user --jq .login 2>/dev/null)}"
[ -n "$OWNER" ] || { log "dep-prs: gh is not signed in"; exit 0; }

# Repos with open dependency PRs, most PRs first.
repos=$(for a in app/dependabot app/renovate; do
    gh search prs --owner "$OWNER" --state open --author "$a" --limit 200 --json repository \
      --jq '.[].repository.name' 2>/dev/null
  done | sort | uniq -c | sort -rn | awk '{print $2}')
[ -n "${BN_DEP_REPOS:-}" ] && repos=$(printf '%s\n' $repos | grep -xF -f <(printf '%s\n' $BN_DEP_REPOS))
[ -n "$repos" ] || { log "dep-prs: no open dependency PRs under $OWNER"; exit 0; }

# Agents of this lane already working, per repo. A live one is left to finish.
live_json=$("$BN" agent live --json 2>/dev/null) || { log "dep-prs: cannot read live agents; skipping"; exit 0; }
live_for() {
  printf '%s' "$live_json" | jq -r --arg r "$1" '[.data.agents[]? | select(.repo == $r)
    | select(.branch | test("^deps[-/]")) | select(.has_window == true)
    | select(.state != "done" and .state != "gone")] | length'
}
running=$(printf '%s' "$live_json" | jq -r '[.data.agents[]? | select(.branch | test("^deps[-/]"))
  | select(.has_window == true) | select(.state != "done" and .state != "gone")] | length')

started=0
for repo in $repos; do
  [ $((running + started)) -lt "$MAX" ] || { log "dep-prs: $MAX agent(s) already live; the rest wait for the next run"; break; }
  [ -f "$HOME/.bn/dep-prs.$repo.off" ] && { log "dep-prs: $repo disabled"; continue; }
  [ "$(live_for "$repo")" = 0 ] || { log "dep-prs: $repo already has an agent"; continue; }
  if [ ! -d "$ROOT/$repo/main" ]; then
    log "dep-prs: cloning $OWNER/$repo"
    (cd "$HOME" && "$BN" wt clone "https://github.com/$OWNER/$repo.git" </dev/null >/dev/null 2>&1) \
      || { log "dep-prs: could not clone $repo; skipping it"; continue; }
  fi
  task="Work the open dependency PRs of $OWNER/$repo."
  [ -n "${BN_DEP_REPORT_TO:-}" ] && task="$task Report to $BN_DEP_REPORT_TO with bn agent msg when the pass ends."
  set -- --repo "$repo" --skill dep-prs --branch "deps/$(date +%Y%m%d-%H%M)" \
         --name "deps $repo" --fresh --yolo --agent
  [ -n "${BN_DEP_AGENT:-}" ] && set -- "$@" "$BN_DEP_AGENT"
  # `--` keeps an optional --agent value from swallowing the task text.
  set -- "$@" -- "$task"
  if [ "${BN_DEP_DRYRUN:-0}" = 1 ]; then
    log "DRYRUN: $BN dispatch $*"
  elif "$BN" dispatch "$@"; then
    log "dep-prs: dispatched an agent on $repo"
  else
    log "dep-prs: dispatch failed for $repo"; continue
  fi
  started=$((started + 1))
done
exit 0
