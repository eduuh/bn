#!/bin/sh
# red-main -- a red default branch gets an agent within half an hour.
#
# Fired by the scheduler (agents/red-main.md), opt-in on a worker machine. For every repo you own
# that was pushed to recently, it reads the latest completed run of each workflow on the default
# branch. A red one nobody is fixing gets one agent, seeded with the `red-main` skill and a
# checklist of the failing runs, which reproduces the failure and opens a fix PR. A green repo
# costs one `gh run list`.
#
# It never fixes the same failing commit twice: a (repo, workflow, sha) it dispatched for is
# remembered in ~/.cache/bn/red-main.seen, so an agent that could not fix it does not loop.
#
# Settings (env, or ~/.config/bn/red-main.env):
#   BN_RED_MAIN_AGENT  harness                        (default: the configured one)
#   BN_RED_MAX         agents of this lane live at once (default: 1)
#   BN_RED_DAYS        only repos pushed within N days (default: 30)
#   BN_RED_IGNORE      workflow names never to act on, '|'-separated
#                      (default: "Dependabot Updates", GitHub's own job, not your CI)
#   BN_LANE_REPORT_TO  repo:branch to tell when an agent starts
#   BN_LANE_DRYRUN=1   print instead of dispatching
# Kill switch: ~/.bn/red-main.off
set -u
. "$(dirname "$0")/lib/lanes.sh"
lane_init red-main

MAX="${BN_RED_MAX:-1}"
DAYS="${BN_RED_DAYS:-30}"
IGNORE="${BN_RED_IGNORE:-Dependabot Updates}"
SEEN="$HOME/.cache/bn/red-main.seen"
mkdir -p "$(dirname "$SEEN")"; touch "$SEEN"

since=$(date -u -d "-$DAYS days" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-"$DAYS"d +%Y-%m-%dT%H:%M:%SZ)
repos=$(gh repo list "$OWNER" --limit 200 --no-archived --source --json name,pushedAt,defaultBranchRef \
  --jq ".[] | select(.pushedAt > \"$since\") | \"\(.name) \(.defaultBranchRef.name)\"" 2>/dev/null)
[ -n "$repos" ] || { lane_log "red-main: no recently pushed repos under $OWNER"; exit 0; }

running=$(lane_live redmain)
started=0
printf '%s\n' "$repos" | while read -r repo branch; do
  [ -n "$repo" ] || continue
  # The latest completed run of each workflow on the default branch; the red ones, minus ignores.
  red=$(gh run list -R "$OWNER/$repo" --branch "$branch" --limit 40 \
      --json workflowName,conclusion,status,createdAt,headSha,url 2>/dev/null \
    | jq -r --arg ign "$IGNORE" '
        [.[] | select(.status == "completed")] | group_by(.workflowName)[] | max_by(.createdAt)
        | select(.conclusion == "failure" or .conclusion == "timed_out" or .conclusion == "startup_failure")
        | select(.workflowName as $w | ($ign | split("|") | index($w)) | not)
        | "\(.workflowName)\t\(.headSha)\t\(.url)\t\(.createdAt)"')
  [ -n "$red" ] || continue
  # Already handled: every red (workflow, sha) here was dispatched for before.
  fresh=$(printf '%s\n' "$red" | while IFS="$(printf '\t')" read -r wf sha url at; do
    grep -qxF "$repo	$wf	$sha" "$SEEN" || printf '%s\t%s\t%s\t%s\n' "$wf" "$sha" "$url" "$at"
  done)
  [ -n "$fresh" ] || { lane_log "red-main: $repo red, already dispatched for these commits"; continue; }
  [ "$(lane_live redmain "$repo")" = 0 ] || { lane_log "red-main: $repo already has an agent"; continue; }
  # A fix PR already open from an earlier run: let it land.
  open=$(gh pr list -R "$OWNER/$repo" --state open --json headRefName --jq '[.[] | select(.headRefName | startswith("redmain/"))] | length' 2>/dev/null || echo 0)
  [ "$open" = 0 ] || { lane_log "red-main: $repo has a fix PR open"; continue; }
  [ $((running + started)) -lt "$MAX" ] || { lane_log "red-main: $MAX agent(s) live; $repo waits for the next run"; continue; }
  lane_clone "$repo" || { lane_log "red-main: could not clone $repo"; continue; }

  steps=$(printf '%s\n' "$fresh" | while IFS="$(printf '\t')" read -r wf sha url at; do
    printf '%s red on %s at %s (%s): reproduce, fix, open a PR\n' "$wf" "$branch" "$(printf '%s' "$sha" | cut -c1-8)" "$url"
  done)
  if lane_dispatch "$repo" "redmain/$(lane_stamp)" red-main \
      "Make $OWNER/$repo's $branch green again." "$steps"; then
    [ "${BN_LANE_DRYRUN:-0}" = 1 ] || printf '%s\n' "$fresh" | while IFS="$(printf '\t')" read -r wf sha url at; do
      printf '%s\t%s\t%s\n' "$repo" "$wf" "$sha" >> "$SEEN"
    done
    lane_report "red-main: $repo $branch is red ($(printf '%s\n' "$fresh" | cut -f1 | paste -sd, -)); an agent is on it"
    started=$((started + 1))
  fi
done
exit 0
