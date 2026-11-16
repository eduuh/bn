---
name: plan
description: "Turn a goal into a verifiable implementation plan file, optionally with a visual Lavish pass. Use when explicitly asked for an executable implementation plan or dispatched as the pipeline Plan stage, not for routine edits, standalone diagrams, explanations or reports."
when: An executable implementation plan was requested, ad hoc or through a dispatched Plan stage. Routine coding and standalone diagrams or reports do not require this workflow.
keywords: [lavish, visualize, mermaid, diagram, plan, break down, decompose, scope, design, approach, acceptance criteria, done when, todos, kick off work]
arguments:
  - { name: topic, description: What to plan. Omit to use the assigned task., required: false }
---
Topic: {{topic}} (otherwise the branch goal).

1. Read the assigned task's context, existing findings and any supplied planning gate. Plan only
   when an implementation plan was requested, not for routine edits or standalone diagrams/reports.
2. Render `bn skills render plan-workflow`, passing topic through `--arg`. Follow its
   plan-artifact contract and use its visual mode only if requested.
3. For an explicitly requested investigate-then-reviewed-plan pass, instead render
   `bn skills render investigate-and-plan` with task and url arguments from the request.
   Follow its research, review submission and STOP contract; do not implement after submission.
4. Keep the durable plan at the path `workspan plan init` reports, independent of session history.
   Record decisions and unresolved questions, then report readiness.
