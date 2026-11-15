---
name: build-from-plan
description: "Execute the active plan file, burning it down to its done-when criteria. Use when a plan already exists and you are implementing it in this session."
when: A plan already exists in plans/active.md (or, during migration, in note todos/done-when) and you're ready to build it. The execution loop that turns the checklist into shipped, verified work.
keywords: [work, execute, do, build, implement, burn down, run the plan, next todo, ship, close out]
arguments:
  - { name: continuous, description: "If truthy (true/yes/1), loop through every todo autonomously; otherwise do the next one and stop for review.", required: false }
---
Continuous: {{continuous}} (true/yes/1 loops; otherwise one item then review).

1. Read `bn plan status` and the plan path it reports; the plan lives beside the branch
   note, not in the source checkout. Follow existing approval gates.
2. Render `bn skills render build-from-plan-workflow`, passing continuous through `--arg`,
   and follow its detailed execution and completion contract.
3. Implement only the next approved item; record progress when it actually completes.
   Stop on a changed scope, missing approval or invalid plan.
4. Run `bn verify` and check every done-when criterion before claiming the goal complete.
   Reverify after rebasing onto a changed base.
