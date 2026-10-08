---
name: worktree-sweep
description: Sweep stale worktrees, pool slots and leftover tmux windows across every repo — reclaim what is provably finished, keep the rest with a reason
when: Worktrees, pool slots or tmux windows have piled up — merged branches still checked out, leaked leases, dead agent scratch dirs, a session full of finished cron windows — and you want the disposable ones gone without losing work.
keywords: [worktree, worktrees, stale, sweep, cleanup, reap, prune, reclaim, pool, slot, tmux, session, window, leftover, garbage collect, gc, disk, wt]
arguments:
  - { name: scope, description: "Repo to sweep. Omit (or `all`) to sweep every repo.", required: false }
  - { name: dry_run, description: "If set (true/yes/1), classify and report only — remove nothing.", required: false }
---
You are sweeping **stale worktrees and the tmux they leave behind**: checkouts whose work is done,
pool leases that leaked, scratch dirs whose agent died a week ago, and the windows and sessions
still sitting on all of it. The point is to give back disk, pool slots and a readable window list,
not to tidy.

One rule decides every call: **a worktree is disposable only when its work is provably somewhere
else** — merged into main, pushed to a PR, or written into the branch note. Uncommitted changes and
unpushed commits are not somewhere else. Where the evidence is missing, you keep the worktree and
say why. A sweep that loses an hour of someone's work is worse than no sweep at all.

The scheduled `wt-cleanup` cron only reclaims worktrees whose PR is confirmed merged (a merged
pool slot's uncommitted work is committed onto its branch first). Everything else, including
`fleet-reap`, stays **manual on purpose** so automated reclamation can never kill a session or
remove unmerged work out from under someone. You are that manual pass. Do not propose a cron for
`fleet-reap`.

Scope: `{{scope}}` — a repo name, or empty/`all` for every repo. Dry-run: if `{{dry_run}}` is
truthy (`true`/`yes`/`1`), do steps 1–3 and stop before anything removes.

## 1. Inventory before judging

Run all five. Every one is read-only or dry-run by default, and every removal in step 4 has to cite
one of them. Run them from outside any repo so the all-repos scope applies.

| command | what it tells you |
|---------|-------------------|
| `bn wt list` | every registered worktree and its branch (detached ones show no branch) |
| `bn wt status --all --pr` | which are dirty, and which of those have a merged PR (`stuck`) |
| `bn wt reap --all` | dry run: what bn already considers done and safe |
| `bn wt pool prune --all --verbose` | dry run: over-size and orphaned slots, plus leaked leases |
| `bn agent live --json` | which worktrees hold a **live** agent — every one of these is off limits |
| `tmux list-panes -a -F '#{session_name}\t#{window_id}\t#{window_name}\t#{pane_current_command}\t#{pane_dead}'` | every window, and whether its panes are running something or sitting at a shell |

`bn wt status` prints the *branch* (`repo/feat/x`) while `bn wt list` and `bn wt reap` print the
*directory* (`feat-x`). Same worktree, two names — reconcile them before claiming one is missing.

## 2. Classify

| class | evidence | action |
|-------|----------|--------|
| merged ship, clean | PR `MERGED`, nothing uncommitted, nothing ahead | `bn wt reap --all --yes` |
| finished scout | note carries `task: scout` + `agent done`, tree clean, past its grace | same sweep |
| merged but dirty | `bn wt status --pr` marks it `stuck` | **keep** — hand it back in step 6 |
| leaked pool lease | pool prune: `would reclaim leaked lease — nobody is in it` | `bn wt pool prune --all --yes` |
| over-size or orphaned slot | pool prune: `would remove (over-size)` / bare repo gone | same command |
| registration with no directory | listed by `git worktree list`, path does not exist | `bn wt prune` |
| leaked agent scratch | `.agent-<branch>-<token>`, clean, nobody in it, over a day old | `bn wt reap --all --yes` |
| leaked agent scratch **holding work** | reap prints `hold <name> — leaked scratch worktree with dirty/unpushed work in it` | **keep** — step 5 |
| orphaned agent process | in `bn agent live` with no window, or its worktree/PR is gone | `bn run fleet-reap` |
| dead pane or window | `pane_dead=1` (a `remain-on-exit` corpse) | `bn run fleet-reap` |
| finished cron window | in the `machine` session, named after a job, every pane at a shell | step 6 |
| window outliving its worktree | its worktree path no longer exists | step 6 |
| empty repo session | a repo session whose worktrees are all reclaimed, nobody attached | step 6 |

Anything that fits no row is a **keep**. Classify on the evidence you collected, never on a
directory's name or age: a branch untouched for a month with an open PR is someone's parked work.

## 3. The protected set

Never remove, and never argue yourself past this list:

- the worktree you are standing in, and any worktree with a live tmux window or a running agent;
- `main` (the stow source) and `_release-main`;
- an in-size **idle** pool slot — that *is* the pool, warm and waiting — and any leased slot whose
  holder is still alive;
- anything with uncommitted changes, untracked files, or commits that exist nowhere else;
- anything whose PR is open, or that has no PR at all (no PR means the work never left this disk);
- anything whose PR state came back `UNKNOWN` — a failed `gh`/`az` lookup is evidence of nothing,
  and being offline is not a reason to delete.

And in tmux specifically:

- **any attached session** (`#{session_attached}` is `1`) — someone is looking at it right now;
- the `planning` session, the orchestrator's cockpit — killing the supervisor is the opposite of
  what a sweep is for;
- any window whose pane is running something other than a shell. A live agent shows its harness
  binary as `pane_current_command`; a shell (`zsh`, `bash`, `sh`, `fish`) is the only safe sign a
  window is finished;
- every repo's `main` window — the shell you come back to, idle by design;
- a window belonging to a worktree that still exists. That window is the worktree's, and it goes
  when the worktree does, through `bn wt cleanup` / `bn wt close` — never by a raw kill.

## 4. Sweep, safest first

Order matters: clear the dead processes first so nothing pulls a checkout out from under a live
agent, then reclaim worktrees, then slots, then registrations.

1. `bn run fleet-reap` — the fleet GC: orphaned agent process trees and dead tmux windows.
   It **acts**; there is no dry run. It honors its own gates (never a working agent, an agent with
   uncommitted work, or the orchestrator). To stop one named agent instead, use
   `bn agent kill <repo>:<branch>`, which also frees its pool slot.
2. `bn wt reap --all --yes`
3. `bn wt pool prune --all --yes`
4. `bn wt prune` (per repo, or from outside a repo for all of them)

Read each command's output before running the next. Two outcomes look like errors and are not:

- `skip <name> — removal failed` after `reap --yes` on a merged worktree means the tree was dirty
  and the gate refused it. The dry run listed it as `would reap` because it only checked the PR.
  The gate is doing its job. Do **not** re-run with `--force`; route that worktree to step 6.
- A pool slot still there after `pool prune --yes` is an in-size warm slot. That is the pool, kept.

Never reach for `bn wt cleanup --force`, `bn wt pool destroy --include-dirty`, or `git worktree
remove --force` on a dirty tree to make a stubborn worktree go away. Those flags discard
uncommitted work, and a worktree that resists removal is usually holding the reason.

## 5. Leaked agent scratch worktrees

`bn pipeline investigate` cuts a detached worktree at
`<worktree-root>/<repo>/.agent-<branch>-<token>` and removes it from the same shell command when the
sub-agent exits. Kill the window, reboot the host, or crash the agent and that removal never runs.

`bn wt reap` collects the empty ones for you: it removes a scratch checkout only when nothing is in
it, nothing is uncommitted, no commit exists on a remote nowhere else, and it is over a day old.
You do not have to do that by hand.

What it will **not** do is remove one holding work, and that is the case worth your attention. When
reap prints

```
hold <name> — leaked scratch worktree with dirty/unpushed work in it
```

a sub-agent died with output that reached neither a note nor a branch, and that output exists on
this disk only. Do not delete it. Look at what is there:

```
git -C <path> status
git -C <path> log --oneline HEAD --not --remotes
```

Then rescue it — commit it onto a branch, or copy the findings into the note the sub-agent was
investigating for — and let the next sweep collect the empty checkout. Report it either way: a
recurring hold means a pipeline stage is losing work, which is a bug worth a task, not a chore.

## 6. Stale tmux windows and sessions

Reclaiming a worktree takes its window with it. Nothing takes the rest, and there are three kinds.

**Finished cron windows.** Older bn ran cron jobs in windows in a `machine` session, and a machine
that ran that version may still have them. When such a job finished, the window stayed, holding
panes parked at a shell prompt. They are not `remain-on-exit` corpses, so `fleet-reap`'s dead-pane
prune never sees them, and they accumulated for as long as the crons ticked. Count them first:

```
tmux list-windows -t machine -F '#{window_name}' | sort | uniq -c | sort -rn
```

A window is finished when **every** pane in it reports a shell as `pane_current_command`:

```
tmux list-panes -t machine -s -F '#{window_id} #{window_name} #{pane_current_command}'
```

Kill only those, by id, one at a time — `tmux kill-window -t @<id>`. Never `kill-session` on
`machine`: closing a session is never the sweep's call.

**A window that outlived its worktree.** Its `pane_current_path` points at a directory that no
longer exists, usually because a reclaim removed the worktree while the window was detached. Confirm
the path is gone and no agent holds it, then `tmux kill-window`.

**An empty repo session.** Every worktree reclaimed, nothing attached, nothing but idle shells
left. `bn agent kill-repo <repo>` is the supported way to drop one, and it is deliberately blunt —
so re-read the protected set before using it, and prefer leaving a session alone when in doubt. A
session costs almost nothing; a session killed under someone's work costs an afternoon.

Work window by window and count what you killed. If the same class keeps coming back — and finished
cron windows will, because nothing collects them — that is a bug in the job that spawns them, not a
chore you owe every week. Say so in the report and file it with `bn task add`.

## 7. Report

State plainly:

- **Reclaimed** — each worktree and slot removed, with the evidence that made it safe.
- **Kept** — each candidate you did not remove and the specific reason (dirty, open PR, live agent,
  unknown PR state). Kept is a result, not a failure.
- **Yours to finish** — every merged-but-dirty worktree, by path and change count, with the command
  that resolves it (`git -C <path> status`, then commit, stash, or `bn wt cleanup <path> --force`
  once you have read the diff). Do not make that call for the user.
- **Slots handed back** — how many pool slots returned to warm, since that is what the next
  `bn dispatch` actually consumes.
- **Windows closed** — how many, and of which kind. A large finished-cron count is a finding, not
  an achievement: it means the number will be just as large next week.

If the sweep turned up a leak class nothing collects automatically, record it once with
`bn ref append conventions --repo --section gotchas "<fact> (source, date)"`, and if it is worth fixing in bn itself file it with `bn task add` rather than
sweeping it by hand every week.
