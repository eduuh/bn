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
# Only the listed repos. POSIX on purpose: the scheduler runs jobs with sh (dash on Ubuntu), where a
# bash process substitution here failed the whole lane with a syntax error (2026-10-09).
if [ -n "${BN_DEP_REPOS:-}" ]; then
  only=""
  for r in $repos; do case " $BN_DEP_REPOS " in *" $r "*) only="$only $r" ;; esac; done
  repos=$only
fi
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

# checklist <repo> -- one deterministic line per open dependency PR: its number, bump kind, CI and
# merge state, and the action those facts call for. Computed here, not by the agent, so every run
# follows the same steps and a supervisor can check the agent's note against them.
checklist() {
  for a in app/dependabot app/renovate; do
    gh pr list -R "$OWNER/$1" --state open --author "$a" --limit 100 \
      --json number,title,mergeable,statusCheckRollup 2>/dev/null
  done | jq -rs '
    def major(v): (v | ltrimstr("v") | split(".")[0]);
    add // [] | sort_by(.number) | .[] |
    (.title | capture("from (?<a>[^ ]+) to (?<b>[^ ]+)") // null) as $v |
    (if $v != null then (if major($v.a) != major($v.b) then "major" else "minor" end)
     elif (.title | test("minor-and-patch"; "i")) then "minor"
     else "check-body" end) as $kind |
    ([.statusCheckRollup[]? | (.conclusion // .state // "PENDING")] | map(ascii_upcase)) as $c |
    (if ($c | any(. == "FAILURE" or . == "ERROR" or . == "TIMED_OUT")) then "red"
     elif ($c | any(. == "PENDING" or . == "IN_PROGRESS" or . == "QUEUED" or . == "EXPECTED")) then "pending"
     elif ($c | any(. == "CANCELLED")) then "cancelled"
     else "green" end) as $ci |
    (if .mergeable == "CONFLICTING" then "resolve the conflict: close it if main already has these versions, else @dependabot rebase or merge main into it yourself"
     elif $kind == "major" then (if $ci == "green" then "comment the breaking changes, leave for a human" else "make CI green, comment, leave for a human" end)
     elif $kind == "check-body" then "read the versions table: major -> leave for a human; else as minor"
     elif $ci == "green" then "merge --squash --delete-branch"
     elif $ci == "pending" then "wait for CI, then merge"
     elif $ci == "cancelled" then "re-run CI (gh run rerun), then merge when green"
     else "fix CI on the branch, then merge" end) as $do |
    "#\(.number) [\($kind), ci=\($ci), \(.mergeable | ascii_downcase)] \($do) -- \(.title)"'
}

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
  steps=$(checklist "$repo")
  task="Work the open dependency PRs of $OWNER/$repo. Your checklist is already in your branch note as todos, one per PR, computed from GitHub: follow each line's action and bn done it when finished."
  [ -n "${BN_DEP_REPORT_TO:-}" ] && task="$task Report to $BN_DEP_REPORT_TO with bn agent msg when the pass ends."
  branch="deps/$(date +%Y%m%d-%H%M)"
  set -- --repo "$repo" --skill dep-prs --branch "$branch" \
         --name "deps $repo" --fresh --yolo --one-shot --agent
  [ -n "${BN_DEP_AGENT:-}" ] && set -- "$@" "$BN_DEP_AGENT"
  # `--` keeps an optional --agent value from swallowing the task text.
  set -- "$@" -- "$task"
  if [ "${BN_DEP_DRYRUN:-0}" = 1 ]; then
    log "DRYRUN: $BN dispatch $*"
    printf '%s\n' "$steps" | sed 's/^/    todo: /' >&2
  elif "$BN" dispatch "$@"; then
    log "dep-prs: dispatched an agent on $repo"
    printf '%s\n' "$steps" | while IFS= read -r line; do
      [ -n "$line" ] && "$BN" add todo --repo "$repo" --branch "$branch" "$line" >/dev/null 2>&1 \
        || log "dep-prs: could not add todo to $repo/$branch: $line"
    done
  else
    log "dep-prs: dispatch failed for $repo"; continue
  fi
  started=$((started + 1))
done
exit 0
