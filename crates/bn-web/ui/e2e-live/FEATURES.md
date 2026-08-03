# Gaps worth filling

Written after driving every screen of bn/web against real state, reading the agent-control path
end to end, and fixing seven defects in it. These are the gaps that showed up in the *shape* of the
system rather than in any one screen — ordered by how much time each would give back per unit of
work.

Three themes run through them: **bn can see far more than it can act on**, **the dashboard tells
you what happened but rarely what to do**, and **the riskiest button is the least informative**.

---

## 1. Session-addressable agents — unlocks parallel work on one branch

**The gap.** Every write channel is keyed `repo:branch`: the tmux window name, the note's
`agent-session:` record, and all three endpoints (`send`, `relaunch`, `kill`). During this pass
your fleet had **three** live sessions on `bn:perf-web-code-split`. None of them was individually
steerable — and a `kill` could have landed on any one.

I fixed the *harm* (bn now marks those agents `ambiguous` and refuses to act), but the capability is
still missing, and it's a capability you're evidently already reaching for.

**Build.** Give each dispatch a stable short id and name its tmux window `repo:branch#ab12`. Record
`agent-session:` per session rather than latest-wins. Add `bn agent send --session <id>` (and the
same for kill/relaunch) that bypasses the note lookup entirely. bn-web already knows every
`session_id` — it's on the fleet payload and in the detail URL — so the UI change is passing what
it already has.

**Why it pays.** Fan-out on a single branch becomes safe and controllable: three scouts on one
problem, or a worker plus a reviewer, each individually steerable. Today that configuration silently
degrades to "whichever one the note remembers."

---

## 2. "Why is it stuck?" — the highest-value line of text in the system

**The gap.** The fleet classifies an agent `stuck` purely on idle time. But *stuck* has causes, and
the transcript almost always names the cause in its last entry: a permission prompt, a trust dialog,
a tool approval, or a direct question to the human. bn already reads those transcripts — it renders
them in the activity feed — and it already has a `watch` cron whose whole job is to toast on
"agents waiting/blocked/stuck".

What's missing is the join: the fleet card says *20m idle* when it could say *"waiting on: Should I
force-push to rewrite the branch history?"* with the answer box right there.

**Build.** Classify the trailing transcript entry into `waiting-on-question` / `waiting-on-approval`
/ `genuinely-idle`, promote it to a first-class `AgentHealth`, and surface the question text plus a
reply box on the card. The steer channel to answer it already exists.

**Why it pays.** This is the biggest silent time sink in an agent fleet: an agent that asked a
question ten minutes ago and is burning nothing while it waits. Idle time tells you *that* something
is wrong; the question tells you what to type. It is a one-line-of-text feature with a huge ratio.

---

## 3. Land pre-flight — make the irreversible button the most informative one

**The gap.** "Land ready PRs" runs `bn wt merge --all`, which merges **every** open PR for the repo
and — per its own help — "stays the fast, **ungated** batch path". A single `bn wt merge` gates by
default: rebase onto `origin/main`, run the repo's verify gate on that head, "so a red or stale-base
PR can't land (the class that broke main)". The dashboard skips that, for all PRs at once, behind a
`window.confirm` that names nothing and shows no count.

The one action you cannot undo with a click is the one that tells you least about what it's about
to do.

**Build.** A pre-flight panel: the PRs that would merge, each with check status, whether it's
rebased on current main, and its file-overlap group (`bn wt merge --all` already computes overlap
to serialize them). Gated by default, with ungated as an explicit "I'm in a hurry" toggle. Merge
selected, not merge-all-or-nothing.

**Why it pays.** Turns a leap of faith into a decision. And it's the natural home for the
merge-queue state `--all` already computes internally but never shows anyone.

---

## 4. Retire `window.confirm` (13 sites) — the thing blocking #3

**The gap.** Thirteen destructive actions gate on a native `window.confirm`. It blocks the page,
can't be styled, can't show a list or a diff, behaves badly in an iOS standalone PWA, and — as this
test pass proved twice — silently turns a click into a no-op for any automation that doesn't handle
the dialog. The kill confirm can't show you what the agent was doing; the land confirm can't show
you what it will merge.

**Build.** One `<ConfirmDialog>` taking a title, a body (arbitrary JSX — a PR list, a diff, the
agent's last message), and a typed confirm phrase for the genuinely destructive ones.

**Why it pays.** Small, mechanical, and it's the prerequisite for #3 and for a kill button that
shows you what you're about to lose.

---

## 5. ⌘K as a verb palette, not just a noun palette

**The gap.** I made ⌘K reach every screen. It still can't *do* anything — every result navigates.

**Build.** Actions in the same ranked list: `run <cron>`, `land <repo>`, `start <task>`,
`kill <agent>`, `warm <repo>`. The endpoints all exist; this is a dispatch table over them. Route
destructive verbs through #4's confirm.

**Why it pays.** On a phone the rail is a horizontally scrolling bar where roughly half the
destinations sit off-screen with no affordance — the palette is already the fastest input device you
have there, and it currently stops at navigation. "Re-run the failing cron" from your pocket without
finding the card is the shape of the win.

---

## 6. Cost anomaly detection — you have 30 days of data and no alarm on it

**The gap.** Cost is purely retrospective: five tiles, a chart, a breakdown table. But you have
per-day/repo/model/session history, and `/api/insights` and `/api/metrics/trend` already compute
trends. Nothing watches it. An agent stuck in a loop burning 20× its repo's median shows up
tomorrow, on a chart, after the money is gone.

**Build.** A rolling median per repo and per work-class; flag a live session at N× its cohort and
raise it as an attention item. The fleet already carries live `output_tokens` per agent, so this
works mid-session, not just post-ingest. `attention-sync` runs every five minutes and is the natural
host.

**Why it pays.** The one bn signal that costs real money is currently the only one with no reactor
attached. Given how cheap the fold is — you already compute it — the asymmetry is stark.

---

## 7. Duplicate-work detection

**The gap.** Three agents on one branch (#1) is the acute case; the general one is two agents
editing the same files from different branches, which surfaces as a merge conflict hours later.
`bn wt merge --all` already computes changed-file overlap between PRs — the same computation applied
to *in-flight* worktrees would catch the collision while it's still cheap.

**Build.** Diff each live agent's worktree against its base, intersect the changed-file sets
pairwise, and surface overlaps on the console as "these two are editing the same 3 files".

**Why it pays.** Reuses machinery that already exists, and moves conflict discovery from merge time
to work time.

---

## 8. Stuck-agent auto-recovery, now that relaunch tells the truth

**The gap.** Before this pass, relaunch reported success whether or not it recovered anything — so
no automation could safely be built on it. Now that it returns `nothing_to_recover` honestly, a
policy loop is possible.

**Build.** In the `watch` cron: if an agent is stuck > N minutes **and** `control` is `resume` or
`tmux`, attempt one recovery, record the outcome, and escalate to attention only if it fails. The
`control` field I added makes "can I even try?" a cheap local check rather than a subprocess.

**Why it pays.** Closes the loop on the most common failure mode. The honest envelope was the
blocker; it's gone.

---

## 9. A "while you were away" strip

**The gap.** You have `/api/events`, `/api/wrap`, `/api/completed` and a handoff screen — all
excellent for *reconstructing* a period. But coming back to the dashboard after two hours, there's
no answer to "what changed?" short of reading four screens.

**Build.** One line at the top of the console, computed from the event log since your last visit
(localStorage timestamp): *"since 14:20 — 3 agents finished, 6 PRs merged, 2 crons failed, 1 agent
waiting on you"*, each segment a filter into the relevant screen.

**Why it pays.** It is the question everyone actually opens the dashboard to ask, and every input
already exists.

---

## 10. Auth that matches the `--lan` use case

**The gap.** There is no token gate on any write, by design (`#523`) — the trust model is the bind
address. That's defensible on loopback. But `--lan` is a first-class documented flag, and the
phone-PWA story basically requires it; on that path, any device on the network can kill agents,
merge PRs, run crons and rewrite skill files unauthenticated.

**Build.** Keep loopback wide open; require the paired token only when bound off-loopback. The
token already exists, is already persisted at `~/.bn/config/web-token`, and the UI already stores it
in localStorage — the pairing machinery is all there, just unused.

**Why it pays.** Removes the one reason to hesitate before running `--lan`, which is what makes the
dashboard useful from your pocket in the first place.

---

## Smaller things worth a slot

- **Controllability badge on fleet cards.** `control` is now on the payload but only the steer panel
  reads it. One glyph for drive-able vs observed-only would answer "why is that greyed out?" before
  the click.
- **Cron cards stretch to the tallest in the row**, leaving a large hole on jobs with no
  description (`screenshots/cron-desktop.png`). `items-start` on the grid.
- **`/api/repos/{unknown}/issues` returns `200` with an empty list** while every other unknown-id
  endpoint returns a typed `404`. Harmless, but it's the one inconsistency in an otherwise
  impeccable error surface.
- **`by_day` is sorted cost-desc**, like every other `by_*` breakdown. Defensible and documented,
  but for a *time* axis every consumer has to re-sort — worth an exception.
