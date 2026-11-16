---
name: dispatch-plan-work-bootstrap
internal: true
description: "Bootstrap a new build-from-plan dispatch. Use when the assigned task has no plan artifact yet."
---
The task has only a goal and no plan artifact yet, so build-from-plan's "no plan →
stop and recommend `plan`" case does not apply here: write the plan first, then execute it. Run
`bn plan init`, then use `bn plan path` to locate the Markdown execution contract.
Write the goal, 2-5 observable done-when criteria, ordered steps, risks, and open questions there,
then execute the build-from-plan loop from it. Do not duplicate the plan's checklist in a
separate task list or guess its filesystem location.
