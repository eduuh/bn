#!/bin/sh
# pr-babysit -- keep your own open PRs mergeable, and tell you which ones are ready.
#
# Fired nightly by the scheduler (agents/pr-babysit.md), opt-in on a worker machine. It reads every
# open PR you authored under your account and sorts each one, with plain gh and jq:
#   ready     green and mergeable: listed in one report to you, nothing else (merging is yours)
#   conflict  behind main with conflicts: an agent merges main in and resolves them
#   red       CI failing: an agent reproduces and fixes it on the PR's branch
#   pending / draft / quiet-period: left alone
#   tried     an agent already tried it at this PR commit and this base commit: left until either moves
# One agent per repo with work, seeded with the `pr-babysit` skill and a checklist of its PRs.
#
# Hands off what someone is on: a PR updated in the last BN_BABYSIT_QUIET_H hours, or whose branch
# has a live agent here, is skipped. The agent never force-pushes and never merges.
#
# Settings (env, or ~/.config/bn/pr-babysit.env):
#   BN_PR_BABYSIT_AGENT  harness                          (default: the configured one)
#   BN_BABYSIT_MAX       agents of this lane live at once (default: 1)
#   BN_BABYSIT_QUIET_H   skip PRs touched within N hours  (default: 12)
#   BN_LANE_REPORT_TO    repo:branch for the nightly report
#   BN_LANE_DRYRUN=1     print instead of dispatching
# Kill switch: ~/.bn/pr-babysit.off
set -u
. "$(dirname "$0")/lib/lanes.sh"
lane_init pr-babysit

MAX="${BN_BABYSIT_MAX:-1}"
QUIET="${BN_BABYSIT_QUIET_H:-12}"
quiet_since=$(date -u -d "-$QUIET hours" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-"$QUIET"H +%Y-%m-%dT%H:%M:%SZ)
live_branches=$(printf '%s' "$LANE_LIVE" | jq -r '.data.agents[]? | select(.has_window == true) | "\(.repo)\t\(.branch)"')

repos=$(gh search prs --author "$OWNER" --owner "$OWNER" --state open --limit 200 --json repository \
  --jq '.[].repository.name' 2>/dev/null | sort -u)
[ -n "$repos" ] || { lane_log "pr-babysit: no open PRs by $OWNER"; exit 0; }

# One line per PR: repo, number, class, title. Classified here so every run sorts the same way.
rows=$(for repo in $repos; do
  gh pr list -R "$OWNER/$repo" --author "$OWNER" --state open --limit 100 \
    --json number,title,isDraft,mergeable,headRefName,updatedAt,statusCheckRollup,url 2>/dev/null \
  | jq -r --arg repo "$repo" --arg quiet "$quiet_since" '.[] |
      ([.statusCheckRollup[]? | (.conclusion // .state // "PENDING") | ascii_upcase]) as $c |
      (if .isDraft then "draft"
       elif .updatedAt > $quiet then "quiet"
       elif .mergeable == "CONFLICTING" then "conflict"
       elif ($c | any(. == "FAILURE" or . == "ERROR" or . == "TIMED_OUT" or . == "STARTUP_FAILURE")) then "red"
       elif ($c | any(. == "PENDING" or . == "IN_PROGRESS" or . == "QUEUED" or . == "EXPECTED")) then "pending"
       elif .mergeable == "MERGEABLE" then "ready"
       else "unknown" end) as $class |
      "\($repo)\t\(.number)\t\($class)\t\(.headRefName)\t\(.url)\t\(.title)"'
done)

# Never the same PR twice at the same commit: one an agent already tried and could not fix waits
# for a new commit (yours, or main moving) instead of costing a run every night.
SEEN="$HOME/.cache/bn/pr-babysit.seen"
mkdir -p "$(dirname "$SEEN")"; touch "$SEEN"
rows=$(printf '%s\n' "$rows" | while IFS="$(printf '\t')" read -r repo n class head url title; do
  [ -n "$repo" ] || continue
  case "$class" in
    (conflict|red)
      sha=$(gh pr view "$n" -R "$OWNER/$repo" --json headRefOid --jq .headRefOid 2>/dev/null)
      base=$(gh pr view "$n" -R "$OWNER/$repo" --json baseRefName --jq .baseRefName 2>/dev/null)
      main=$(gh api "repos/$OWNER/$repo/commits/$base" --jq .sha 2>/dev/null)
      grep -qxF "$repo#$n $sha $main" "$SEEN" && class=tried ;;
  esac
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$repo" "$n" "$class" "$head" "$url" "$title"
done)

# Skip what an agent here is already on.
rows=$(printf '%s\n' "$rows" | while IFS="$(printf '\t')" read -r repo n class head url title; do
  [ -n "$repo" ] || continue
  hd=$(printf '%s' "$head" | tr '/' '-')
  if printf '%s\n' "$live_branches" | grep -qxF "$repo	$head" || printf '%s\n' "$live_branches" | grep -qxF "$repo	$hd"; then
    class=busy
  fi
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$repo" "$n" "$class" "$head" "$url" "$title"
done)

count() { printf '%s\n' "$rows" | awk -F'\t' -v c="$1" '$3 == c' | grep -c . || true; }
ready=$(printf '%s\n' "$rows" | awk -F'\t' '$3 == "ready" {print $1 "#" $2}' | paste -sd' ' -)

running=$(lane_live babysit)
started=0
for repo in $repos; do
  work=$(printf '%s\n' "$rows" | awk -F'\t' -v r="$repo" '$1 == r && ($3 == "conflict" || $3 == "red")')
  [ -n "$work" ] || continue
  [ "$(lane_live babysit "$repo")" = 0 ] || { lane_log "pr-babysit: $repo already has an agent"; continue; }
  [ $((running + started)) -lt "$MAX" ] || { lane_log "pr-babysit: $MAX agent(s) live; $repo waits"; continue; }
  lane_clone "$repo" || { lane_log "pr-babysit: could not clone $repo"; continue; }
  steps=$(printf '%s\n' "$work" | while IFS="$(printf '\t')" read -r r n class head url title; do
    case "$class" in
      (conflict) act="merge origin/main into $head, resolve, push (no force)" ;;
      (red) act="reproduce the failing check and fix it on $head, push (no force)" ;;
    esac
    printf '#%s [%s] %s -- %s (%s)\n' "$n" "$class" "$act" "$title" "$url"
  done)
  if lane_dispatch "$repo" "babysit/$(lane_stamp)" pr-babysit \
      "Make $OWNER/$repo's open PRs mergeable again, without merging them." "$steps"; then
    started=$((started + 1))
    [ "${BN_LANE_DRYRUN:-0}" = 1 ] || printf '%s\n' "$work" | while IFS="$(printf '\t')" read -r r n class head url title; do
      sha=$(gh pr view "$n" -R "$OWNER/$r" --json headRefOid --jq .headRefOid 2>/dev/null)
      base=$(gh pr view "$n" -R "$OWNER/$r" --json baseRefName --jq .baseRefName 2>/dev/null)
      printf '%s#%s %s %s\n' "$r" "$n" "$sha" "$(gh api "repos/$OWNER/$r/commits/$base" --jq .sha 2>/dev/null)" >> "$SEEN"
    done
  fi
done

summary="pr-babysit: ready to merge: ${ready:-none}. conflicting $(count conflict), red $(count red), pending $(count pending), drafts $(count draft), touched <${QUIET}h $(count quiet), agent busy $(count busy), already tried $(count tried); dispatched $started"
lane_log "$summary"
lane_report "$summary"
exit 0
