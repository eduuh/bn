---
name: dispatch-completion
internal: true
description: Standard completion protocol for a dispatched workflow without a plan-review gate
arguments:
  - { name: workflow_name, description: Selected workflow name., required: true }
---
If the selected workflow is read-only research, its deliverable is recorded findings, not a PR.
Follow its evidence and handoff requirements, then
`bn agent done --report-only --summary '<findings and evidence>'`. Do not implement or open an
empty PR. The code-changing completion rules below do not apply to that workflow.

For a code-changing workflow, DELIVERABLE: a PR you validated locally. Before you change anything, REPRODUCE the problem (or
demonstrate the missing behaviour) and record how on the assigned task — a failing test, a
command with its wrong output, or a behaviour you observed in the running app. After the change,
prove it against that same reproduction on the PR head: the reproduction now passes and
`bn verify` is green. A green gate alone validates nothing that nobody reproduced.

When the workflow completes green: push the exact gated commit, open the PR, then run
`bn agent done --summary '<what you did, and how the reproduction now passes>' --pr <url>`.

For code-changing workflows, the ONLY finish without a PR is an issue that cannot be reproduced. Record every attempt with
the assigned task's context/provider, then run `bn agent done --report-only --summary 'not reproducible: <what you tried>'`.
Anything else that stops you short of a PR — a question only a person can answer, a scope that
turned out wrong — is a blocker, not a finish: record the concrete blocker on the task, then
`bn agent done --failed --summary '<what blocks>'`.

Before reporting, record one workflow reflection with
`bn reflect "skill:{{workflow_name}} — <what worked or what to change>"`.
