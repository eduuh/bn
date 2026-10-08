---
name: start-session
description: "Orient on a branch: goal, open todos, blockers, first step; can also load the full note or review every branch. Use first when starting, resuming or picking up work."
when: Beginning a work session or picking up a branch — first thing, before you start coding.
keywords: [start-session, status review, cross-branch, load note, start, begin, session, kick off, pick up, resume, get going, set goal, focus]
arguments:
  - { name: focus, description: Optional area to focus this session on, required: false }
---
You are starting a work session on the current branch.

1. Call `bn_context` to orient — goal, open todos, blockers, and PR in one call. Reach for
   `bn_cat` only if you need the full note body.
2. Summarize the open todos and any blockers in 1–2 lines.
3. If a focus was given ({{focus}}), prioritize todos matching it; otherwise propose
   what to work on next from the open todos.
4. State a concrete goal for this session and the first step you'll take. If the note has no
   goal yet (fresh worktree), set one now with `bn_goal` — it timestamps the start of the work.

Keep it short. Then, as you work, record new work with `bn_add` and close todos with `bn_done`
*the moment each one is actually done* — not batched at the end. Those timestamps feed
`bn metrics` (close-out lag, flow time, WIP), so honest, in-the-moment tracking keeps them real.

## Load the full note and investigation files

When you need more than the summary, run `bn --cat` for the note and `bn-investigation-files.sh` for prior investigation files, then update the note as you work: `bn add todo|blocker|decision|research "text"`, and save longer artifacts to `$(bn --path)/filename.md`.

## Across every branch (was status-review)

When the question is where everything stands, not this branch: run `bn summary`, lead with blockers by branch, then a one-line-per-item rundown of open todos (heaviest first), and flag branches that look stalled. For the day's priorities across branches use `morning`.
