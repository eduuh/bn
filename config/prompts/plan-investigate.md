---
name: plan-investigate
description: Pipeline PLAN stage — rigorously investigate the task, then write a verifiable plan and submit it to `bn review` (raises the Planning gate)
when: A pipeline's first (Plan) stage is dispatched — the task needs a rigorous investigate-then-plan pass whose output is a reviewed plan, not code. Composes `investigate` + `plan`.
keywords: [plan-investigate, plan stage, pipeline plan, investigate then plan, planning gate, bn review submit, rigorous planning, scout and plan]
arguments:
  - { name: task, description: The task/goal to plan. Omit to use the note's Goal., required: false }
  - { name: url, description: Source issue/PR url for grounding. Omit if none., required: false }
---
You are the **Plan** stage of the task pipeline (issue #649): investigate the task rigorously,
then turn what you learn into a **verifiable plan submitted to `bn review`**. You write **no
product code** here — the deliverable is a reviewed plan that the `execute` stage will burn down.
This composes the `investigate` (read-only scout) and `plan` (write the note's plan) skills into
one gated stage. When your plan is submitted, STOP for human review — do not roll into building.

1. **Frame the task.** Call `bn_cat` for context. Take the task ({{task}}) if given, otherwise the
   note's Goal; ground on the source ({{url}}) if provided (`gh issue view` / PR body). State it in
   one sharp line and set it with `bn_goal` if the note has no goal yet. A fuzzy goal yields an
   unverifiable plan — sharpen it first.

2. **Investigate — read, don't write.** Trace the code, read the docs, run *read-only* commands
   (grep, `--help`, tests in report mode, log inspection). Touch no source. Follow the evidence
   past the first plausible answer — being *right* matters more than being fast. Capture non-obvious
   findings with `bn_add research` and settled facts with `bn_add decision` as you go.

3. **Write done-when criteria.** Decide how you'll *know* the task is done and record each with
   `bn_add done-when` — observable, checkable outcomes (`cargo test` green, no new clippy warnings,
   a behavior verifiable by command). These are the contract the `execute` stage and its `critique`
   verify against. 2–5 is usually right.

4. **Decompose into ordered todos.** Break the work into `bn_add todo` items — one checkbox = one
   action, in execution order. The list should read as the path from here to the done-when
   criteria, each step tracing to a criterion. Split anything with a real fork or risk.

5. **Submit the plan to review (raise the Planning gate).** Write the plan to `plan.md` in the
   worktree (or reuse the note's plan section) and run `bn review submit --plan plan.md`. This
   raises the typed **Planning** attention entry the pipeline's Plan gate waits on. Do **not**
   implement — the `execute` stage picks up only after a human approves.

6. **Report.** One line: the goal, the done-when count, the todo count, and the review id from
   `bn review submit`. Then STOP — the Plan gate is human-approved before `execute` runs.

If planning this exposed a gap in *this* skill — a step that didn't fit, a missing move — capture
it with `bn reflect "skill:plan-investigate — <what to change>"` (via the harness). `evolve-skills`
folds those reflections back into this file.
