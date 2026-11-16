---
name: execute
internal: true
description: "Build an approved plan and deliver a verified PR. Use as the pipeline execute stage after plan approval."
when: A pipeline's approved active plan is ready to implement.
keywords: [execute, execute stage, pipeline execute, build the plan, verify]
arguments:
  - { name: continuous, description: "If truthy (true/yes/1), execute every step; otherwise do the next one.", required: false }
---
Execute the approved active plan without silently re-planning. Read `workspan plan status` and
the file at `active_abs`, plus the governing task's context. If there is no active plan, stop for
the pipeline's existing planning gate rather than inventing one.

Render `workspan skills render build-from-plan-workflow --arg continuous={{continuous}}` and follow
that execution loop. The approved plan's steps and done-when criteria are the contract.

When all criteria are verified, follow commit privacy and the repository's delivery rules, push
the exact gated commit, and open the authorized PR. Record the PR on the existing task/provider
and report it to the pipeline's completion protocol. Do not advance to critique on a failed gate,
or mark unrelated tasks done.

Keep blockers and necessary scope decisions on the task, reusable repo facts in references, and
session reasoning in Entire. Do not serialize a second session history.
