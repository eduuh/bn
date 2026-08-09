---
name: execute
description: Pipeline EXECUTE stage — burn the approved plan down to its done-when criteria and open a PR, verifying as you go
when: A pipeline's Execute stage is dispatched after its plan was approved — the reviewed plan already exists in `plans/active.md` (or legacy note sections during migration) and it's time to build it, verify it, and open a PR. The `plan-work` loop specialized for the pipeline.
keywords: [execute, execute stage, pipeline execute, build the plan, burn down, plan-work, ship, open a pr, verify, done-when]
arguments:
  - { name: continuous, description: "If truthy (true/yes/1), loop through every todo autonomously; otherwise do the next todo and stop.", required: false }
---
You are the **Execute** stage of the task pipeline: burn the **already-approved**
active plan artifact down to its done-when criteria, then open a PR. The plan is the contract — you
follow it, you don't re-plan. This is the `plan-work` loop as the pipeline's build stage: it
inherits the approved plan's context and produces a verified change plus a PR the `critique` stage
reads. If the plan turns out to be wrong, say so and stop — don't silently improvise a new one.

1. **Load the approved plan.** Run `bn plan status`. If `plans/active.md` exists, read that
   Markdown file and treat it as the contract. Also call `bn_cat` for branch-note
   status/blockers/decisions. If no active plan file exists, fall back to the legacy note
   `done-when`/todo sections during migration; if neither exists, stop and recommend the
   `plan-investigate` stage first.

2. **Take the next todo.** Pick the top open todo and implement exactly it — nothing speculative,
   nothing adjacent. Match the surrounding code; touch only what the todo needs. Every changed line
   should trace to this todo or a done-when criterion. For multi-site or mechanical changes, census
   every site before editing one, then batch the same-shape edits.

3. **Close it the moment it's done.** Call `bn_done` on the todo when it's actually complete — not
   "started", not batched at the end; the timestamp feeds `bn metrics`. As work surfaces new tasks,
   `bn_add todo`; at a real fork or tradeoff, `bn_add decision` before committing to it.

4. **Handle the walls.** Blocked by something external → `bn_add blocker`, surface it, move to the
   next unblocked todo. Hit a genuine *bug* (broken, not just unbuilt) → hand off to the `debug`
   skill rather than debugging inline and losing the thread. Need a fact you don't have → that's an
   `investigate`, not a guess.

5. **Loop or stop.** If continuous ({{continuous}}) is truthy, repeat 2–4 until the todos are
   empty; otherwise do one todo and stop. Never mark the goal done on vibes.

6. **Verify, ingest, then open the PR.** When the todos are empty, run the repo gate — `bn verify`
   (its build/test/lint gate) — and confirm every done-when criterion holds. Re-run it after any
   rebase onto an advanced base. Only when verify passes and the criteria are met, commit only the
   files you changed, run `bn ingest`, push, and open a PR (`gh pr create`), then record it with
   `bn_pr <url>`. If verify fails, that failure is your next todo (or a `debug`
   hand-off) — reopen the loop, don't paper over it. The PR is what the `critique` stage reviews.

7. **Report.** One line: *N todos closed · verify PASS/FAIL · done-when M/M met · PR <url>*. Then
   what's left and the single next action.
