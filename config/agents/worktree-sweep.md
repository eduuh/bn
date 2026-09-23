---
name: worktree-sweep
requires_repo: bn
kind: command
on_calendar: "*-*-01/2 09:00:00"
opt_in: true
network: true
retry_after: 4h
description: "bn — the worktree sweep: rescue at-risk work onto its branch, reclaim only what bn's own gates already prove is finished, and hand back every judgment call (deletes no branch, kills no session; opt-in, [agents] enabled)"
---
Dispatches an agent seeded with the `worktree-sweep` skill. The skill is the process; this manifest
is the schedule and the blast radius.

**Off until you opt in.** This lane does nothing until `worktree-sweep` is named in
`[agents] enabled`. A scheduled lane that commits onto other branches and reclaims pool slots should
never start running on a machine just because that machine upgraded bn.

No `run:` line, deliberately: the agent resolves to the same-named job script,
`config/jobs/worktree-sweep.sh`, which holds the lane's settings. The agent is fired by a timer
through the scheduler, not from your shell, so the script is the one place its settings are always
set and reviewable in git; settings on a `run:` command line silently reached the driver unset.

**Why this is allowed to run unattended when `fleet-reap` is not.**
`config/crons.toml` keeps unmerged reclamation manual so nothing can delete a branch, remove a
worktree, or kill a tmux session out from under active work (the scheduled `wt-cleanup` only
touches merged PRs, and rescue-commits a merged pool slot before returning it). That rule stands, and
this lane does none of those three things. It inverts the order instead: it *rescues first and reclaims second*,
so the destructive step never has anything left to destroy.

The rescue is the whole point. A slot that is "stale" by every signal — lease 12 days dead, nobody
home, agent long gone — routinely still holds uncommitted work, and on this repo that work is
frequently **untracked new modules** that exist on no branch and no remote. `git clean -df`, which
every reclaim path runs, deletes exactly those. So the agent commits what it finds onto the branch
already checked out in the slot before anything reclaims it. A commit on a branch survives the
worktree that made it, because worktrees share one object store — no push required, though pushing
is better.

This is not hypothetical. The sweep that produced this lane found five bn slots holding 62
uncommitted files, including a half-finished `tat.rs` split and four untracked `wt_pool/` modules.
The slots were reclaimed by a concurrent pass *minutes after* the rescue commits landed. Without
them, that work would be gone with no trace and no way to know what was lost.

**The boundary.** Reclaim only through `bn wt reap` and `bn wt pool prune`, whose own gates refuse a
dirty tree — never `--force`, never `--include-dirty`, never `git worktree remove --force`, and
never `bn wt cleanup` (it deletes branches) or `bn run fleet-reap` (it kills sessions). A worktree
that resists removal is holding the reason. Anything the skill's protected set covers — a live
agent, an open PR, an in-size warm slot, an `UNKNOWN` PR lookup — is kept, and kept is a result,
not a failure.

Report what was rescued, what was reclaimed, and what is left for a human to decide. If the same
leak class keeps reappearing, that is a bug in whatever spawns it: file it with `bn task add`
rather than sweeping it by hand every other day.

**Schedule.** `*-*-01/2` fires on odd days of the month, so the turn of a 31-day month gives two
runs a day apart. The stepped day-of-month is inside the `OnCalendar` subset `crate::schedule`
understands, the same parser the due check uses, and
`a_stepped_day_of_month_fires_on_odd_days_only` pins both the firing and that support.
