---
name: investigate-and-plan
description: "Investigate a task then write the plan in one pass and submit it to bn review, raising the Planning gate. Use for an ad-hoc task that needs both research and a plan."
when: A task needs a rigorous investigate-then-plan pass whose output is a reviewed plan, not code — dispatched ad hoc via `--skill investigate-and-plan` (the pipeline's own Plan stage runs the `plan` skill). Composes `investigate` in pipeline/read-only mode + `plan`.
keywords: [investigate-and-plan, investigate then plan, planning gate, bn review submit, rigorous planning, scout and plan, reviewed plan]
arguments:
  - { name: task, description: The task/goal to plan. Omit to use the note's Goal., required: false }
  - { name: url, description: Source issue/PR url for grounding. Omit if none., required: false }
---
Investigate the task rigorously, then turn what you learn into a **verifiable plan submitted to
`bn review`**. You write **no product code** here: the deliverable is a reviewed plan an execution
lane will burn down. Once the plan is submitted, STOP for human review.

1. **Frame the task.** Call `bn_cat` for context. Take the task ({{task}}) if given, otherwise the
   note's Goal; ground on the source ({{url}}) if provided (`gh issue view` / PR body). State it in
   one sharp line and set it with `bn_goal` if the note has no goal yet. A fuzzy goal yields an
   unverifiable plan — sharpen it first.

2. **Investigate — read, don't write.** Trace the code, read the docs, run *read-only* commands
   (grep, `--help`, tests in report mode, log inspection). Touch no source. Follow the evidence
   past the first plausible answer — being *right* matters more than being fast. Capture non-obvious
   findings with `bn_add research` and settled facts with `bn_add decision` as you go.
   Delegate substantial evidence gathering to Copilot fleet / Claude subagents using bounded
   scoping questions. Keep one planner responsible for synthesis and the plan artifact. Workers
   return paths, sources, reproduction evidence, and unresolved questions; they do not write
   competing plans or launch another layer. Keep cheap validation of a known fact local.

3. **Write the plan artifact.** Run `bn plan init`, then write the file `bn plan path` prints
   (`plans/active.md` under the branch note directory, not the worktree): the goal in one
   line; 2–5 **done-when criteria** — observable, checkable outcomes (this repo's test gate green,
   no new lint warnings, a behavior verifiable by command — named as the commands this repo
   actually runs) that the execution lane and its critique verify against; **ordered steps** — one action each, the path from here to the criteria, each
   step tracing to a criterion, anything with a real fork or risk split out; risks and open
   questions. Do not decompose the plan through `bn_add todo`/`bn_add done-when` calls — the
   artifact is the contract.

4. **Submit the plan to review (raise the Planning gate).** Run
   `bn review submit --repo <repo> --branch <branch> --plan-file "$(bn plan path)"`
   (repo and branch are the current worktree's; `bn_status` has both). It prints the review id and
   raises the **Planning** attention entry the Plan gate waits on. If it fails, report its error and
   stop. Do **not** implement: execution picks up only after a human approves.

5. **Report.** One line: the goal, the done-when count, the step count, and the review id. Then STOP.
