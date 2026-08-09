---
name: start-session
description: Begin a branch-notes work session — review open todos & blockers, set a goal
when: Beginning a work session or picking up a branch — first thing, before you start coding.
keywords: [start, begin, session, kick off, pick up, resume, get going, set goal, focus]
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
