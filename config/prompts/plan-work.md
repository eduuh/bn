---
name: plan-work
description: Execute the active plan artifact — burn it down to done-when criteria, verifying as you go
when: A plan already exists in `plans/active.md` (or, during migration, in note todos/done-when) and you're ready to build it. The execution loop that turns the checklist into shipped, verified work.
keywords: [work, execute, do, build, implement, burn down, run the plan, next todo, ship, close out]
arguments:
  - { name: continuous, description: "If truthy (true/yes/1), loop through every todo autonomously; otherwise do the next one and stop for review.", required: false }
---
You are executing the active plan artifact — burning its steps down to the done-when criteria. The
plan is the contract; you follow it, you don't re-plan. If it turns out to be wrong, say so and
stop — don't silently improvise a different one.

1. **Load the plan.** Run `bn plan status`. If `plans/active.md` exists, read that Markdown file
   and treat it as the contract: its `## Steps` are your ordered worklist and its `## Done When`
   criteria the finish line. Also call `bn_cat` for branch-note status/blockers/decisions — open
   note todos are additional work items. If only legacy note `done-when`/todo sections exist, run
   `bn plan migrate` first (it builds `plans/active.md` from those sections deterministically). If
   neither exists, stop and recommend running `plan` — unless this seed told you to write the
   plan yourself, in which case do that.

2. **Take the next open item** — the first unchecked plan step, else the top open note todo.
   Implement exactly it — nothing speculative, nothing adjacent (match the surrounding code,
   touch only what the item needs). Every changed line should trace to this item or a done-when
   criterion. When the item is a multi-site or mechanical change, **census first**: read *every*
   site before you edit one, locating issue-cited spots by content (line numbers drift), then
   batch the same-shape edits. If a lint or guard enforces the change, run it live and let its
   shrinking hit-list (100→0) be your worklist.

3. **Close it the moment it's done.** Check the step off in `plans/active.md` (or `bn_done` the
   todo) *when it's actually complete* — not "started", not batched at the end. Todo timestamps
   feed `bn stats metrics`, so honesty keeps the numbers real. As work surfaces new tasks, add plan
   steps or `bn_add todo`; if you hit a real fork or tradeoff not already decided, `bn_add
   decision` before you commit to it.

4. **Handle the walls.** Blocked by something external → `bn_add blocker`, surface it, move to the
   next unblocked todo. Hit a *bug* — something broken, not just unbuilt → stop the loop and hand
   off to the `debug` skill; don't debug inline and lose the thread. Need a fact you don't have →
   that's an `investigate`, not a guess.

5. **Loop or stop.** If continuous ({{continuous}}) is truthy, repeat 2–4 until every plan step
   and todo is closed. Otherwise do one item and stop for review. Either way, never mark the
   *goal* done on vibes.

6. **Verify before you claim done.** When the steps and todos are empty, run the repo's gate — `bn verify`
   (its build/test/lint gate) via the harness — and check every `done-when` criterion actually
   holds. Only if verify passes *and* the criteria are met is the work done. If verify fails, that
   failure is your next todo (or a `debug` hand-off) — reopen the loop, don't paper over it. If you
   rebased onto a moved base after verifying, **re-run `bn verify`** — green on the old base doesn't
   survive origin/main advancing. For a lint/guard todo, **prove the gate bites**: plant one probe
   violation, confirm it trips, then revert it — a guard you never saw fail isn't verified.

7. **Learn and report.** After the verified work is committed, run `bn ingest` to fold the shipped
   outcome back. Anything you learned about **the repo itself** — how it builds, where a subsystem lives, a
   failure that misleads — outlives this branch. Record it with `bn_ref_append` (one fact, with
   its source and today's date; see `/bn:repo-reference-update`), not only in the branch note,
   which disappears when the branch does.
 Then report one line: *N todos closed · verify PASS/FAIL · done-when M/M met*,
   followed by what's left and the single next action. When it's genuinely all green, say so
   plainly, then `wrap-session`.
