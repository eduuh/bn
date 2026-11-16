---
name: plan-workflow
internal: true
description: "Write a verifiable implementation plan, optionally with a visual pass. Use when an executable plan is explicitly requested or dispatched."
when: An implementation plan was requested or a pipeline selected its Plan stage.
keywords: [plan, acceptance criteria, done when, design, lavish, visualize]
arguments:
  - { name: topic, description: Goal to plan; otherwise use the assigned task., required: false }
---
Produce a plan artifact, not code. Routine coding, explanations, diagrams, and reports do not
require this workflow.

1. **Orient.** Use the topic ({{topic}}), otherwise the assigned task and its context. Read supplied
   findings before investigating missing details. Do not repeat settled research or invent
   evidence. Ask if the goal is unclear.
2. **Locate the artifact.** Use the draft file named by the seed, otherwise run `workspan plan init`
   and use the path it reports. Do not guess its filesystem location.
3. **Write the contract.** Include a one-line goal, 2-5 observable done-when criteria with this
   repo's actual commands, ordered steps, risks, and open questions. Keep necessary tradeoff
   decisions with their rationale in the plan. Do not duplicate its checklist in a task list.
4. **Resolve unknowns.** Ask on blockers. For a dispatched pipeline, follow its supplied return
   to investigation; otherwise report the concrete unanswered question and stop. Do not change
   pipeline state merely because planning encountered uncertainty.
5. **Report readiness.** Give the artifact path, whether it is ready to execute, and the first
   step or specific blocker. The plan holds the detail; Entire holds session history.

## Optional visual pass

Only when requested, visualize the implementation plan with Lavish before finalizing it.
Use `lavish-axi design` for guidance and `npx -y lavish-axi <file.html>` to open the artifact.
If unavailable, report the skipped visual pass and retain the text plan; do not install packages
ad hoc. Iterate on annotations until the plan is agreed, then put the agreed contract in the plan
file. Export the visual artifact to the plan's artifact directory and reference it from the plan,
which must remain executable without opening the HTML.
