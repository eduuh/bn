#!/usr/bin/env bash
# worktree-sweep — the every-other-day reclamation pass. Rescues first, reclaims second.
#
# The per-lane settings live HERE rather than on the agent manifest's `run:` line, and that is the
# whole point of this file: `bn run <job>` is captured by the queue and re-dispatched later in a
# fresh process, which inherits none of the environment the original command line set.
#
# Blast radius: this lane deletes no branch, removes no dirty worktree and kills no tmux session.
# It reclaims only through `bn wt reap` and `bn wt pool prune`, both of which refuse a dirty tree on
# their own. `--force` and `--include-dirty` are off the table by design, not by omission — see
# config/agents/worktree-sweep.md for why this is allowed to run unattended at all.
set -u

BN="${BN:-bn}"
BN_SWEEP_REPO="${BN_SWEEP_REPO:-all}"
BN_SWEEP_DRYRUN="${BN_SWEEP_DRYRUN:-0}"

# A kill switch that matches the explore lane's, so one habit turns off either.
if [ -f "$HOME/.bn/sweep.off" ]; then
  echo "worktree-sweep: disabled by ~/.bn/sweep.off"
  exit 0
fi

branch="sweep/$(date +%Y%m%d-%H%M)"

assignment="Run the worktree sweep over ${BN_SWEEP_REPO} following the worktree-sweep skill exactly.

Order is load-bearing and is the reason this lane may run unattended at all:

1. INVENTORY first, and judge only on what it returns. Every command in the skill's step 1 is
   read-only or a dry run.
2. RESCUE before anything reclaims. For every worktree or pool slot that is stale by the skill's
   evidence but still holds uncommitted or untracked files, commit them onto the branch already
   checked out there, with a wip( ) subject saying they were rescued by the sweep and are
   unreviewed. Untracked new files matter most: they exist on no branch and no remote, and the
   'git clean -df' inside every reclaim path is what destroys them. A commit on a branch outlives
   the worktree that made it because worktrees share one object store. Push where a remote exists.
   Report the commit SHA per branch so a human can reset or rebase it.
3. RECLAIM only through 'bn wt reap' and 'bn wt pool prune', whose gates refuse a dirty tree. Never
   pass --force or --include-dirty, never run 'git worktree remove --force', and never invoke
   'bn wt cleanup' or 'bn run fleet-reap' from this lane: the first deletes branches, the second
   kills sessions, and config/crons.toml bans both from running unattended.
4. KEEP everything the skill's protected set covers, and say why for each one. Kept is a result.

Then report: what was rescued (branch and SHA), what was reclaimed and the evidence that made it
safe, how many pool slots went back to warm, and what is left for a human to decide. If a leak
class keeps recurring, file it with 'bn task add' instead of sweeping it by hand every other day."

set -- --repo bn --skill worktree-sweep --branch "$branch" \
       --name "worktree sweep" --fresh --agent

if [ "$BN_SWEEP_DRYRUN" = "1" ]; then
  echo "DRYRUN: $BN dispatch $* -- \"$assignment\""
  exit 0
fi

# `--` before the assignment is load-bearing: `--agent` takes an OPTIONAL value, so an unpinned
# harness would swallow the assignment text as the harness name.
if "$BN" dispatch "$@" -- "$assignment"; then
  echo "worktree-sweep: dispatched on bn/$branch (scope ${BN_SWEEP_REPO})"
  exit 0
fi

echo "worktree-sweep: dispatch failed"
exit 1
