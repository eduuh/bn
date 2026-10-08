---
name: execute
description: "Run the approved plan to its done-when criteria and open a PR, verifying as you go. Use as the pipeline execute stage once a plan is approved."
when: A pipeline's Execute stage is dispatched after its plan was approved — the reviewed plan already exists in `plans/active.md` (or legacy note sections during migration) and it's time to build it, verify it, and open a PR. The `build-from-plan` loop specialized for the pipeline.
keywords: [execute, execute stage, pipeline execute, build the plan, burn down, build-from-plan, ship, open a pr, verify, done-when]
arguments:
  - { name: continuous, description: "If truthy (true/yes/1), loop through every todo autonomously; otherwise do the next todo and stop.", required: false }
---
You are the **Execute** stage of the task pipeline: burn the **already-approved**
active plan artifact down to its done-when criteria, then open a PR. The plan is the contract — you
follow it, you don't re-plan. This is the `build-from-plan` loop as the pipeline's build stage: it
inherits the approved plan's context and produces a verified change plus a PR the `critique` stage
reads. If the plan turns out to be wrong, say so and stop — don't silently improvise a new one.

1. **Load the approved plan.** Run `bn plan status`. `plans/active.md` lives in the branch note
   dir, not the repo: `active_abs` is its full path, `active_exists` says whether it exists. If it
   does, read it and treat it as the contract: its `## Steps` are your ordered worklist and its
   `## Done When` criteria the finish line. Also call `bn_cat` for branch-note
   status/blockers/decisions — open note todos are additional work items. If only legacy note
   `done-when`/todo sections exist, run `bn plan migrate` first (it builds `plans/active.md` from
   them deterministically); if neither exists, stop and recommend the `investigate-and-plan` stage —
   unless a plan gate in this seed owns the plan lifecycle, in which case follow the gate.

2. **Take the next open item** — the first unchecked plan step, else the top open note todo —
   and implement exactly it: nothing speculative, nothing adjacent. Match the surrounding code;
   touch only what the item needs. Every changed line should trace to this item or a done-when
   criterion. For multi-site or mechanical changes, census every site before editing one, then
   batch the same-shape edits.

3. **Close it the moment it's done.** Check the step off in the plan file at `active_abs` (or `bn_done`
   the todo) when it's actually complete — not "started", not batched at the end; todo timestamps
   feed `bn stats metrics`. As work surfaces new tasks, add plan steps or `bn_add todo`; at a real fork
   or tradeoff, `bn_add decision` before committing to it.

4. **Handle the walls.** Blocked by something external → `bn_add blocker`, surface it, move to the
   next unblocked todo. Hit a genuine *bug* (broken, not just unbuilt) → hand off to the `debug`
   skill rather than debugging inline and losing the thread. Need a fact you don't have → that's an
   `investigate`, not a guess.

5. **Loop or stop.** If continuous ({{continuous}}) is truthy, repeat 2–4 until every plan step
   and todo is closed; otherwise do one item and stop. Never mark the goal done on vibes.

6. **Verify, ingest, then open the PR.** When the steps and todos are empty, run the repo gate — `bn verify`
   (its build/test/lint gate) — and confirm every done-when criterion holds. Re-run it after any
   rebase onto an advanced base. Only when verify passes and the criteria are met, commit only the
   files you changed, run `bn ingest`, push, and open a PR (`gh pr create`), then record it with
   `bn_pr <url>`. If verify fails, that failure is your next todo (or a `debug`
   hand-off) — reopen the loop, don't paper over it. The PR is what the `critique` stage reviews.

7. **Record what outlives the branch.** Anything you learned about **the repo itself** — how it builds, where a subsystem lives, a
failure that misleads — outlives this branch. Record it with `bn_ref_append` (one fact, with
its source and today's date; see `/bn:repo-reference`), not only in the branch note,
which disappears when the branch does.

8. **Report.** One line: *N todos closed · verify PASS/FAIL · done-when M/M met · PR <url>*. Then
   what's left and the single next action.
