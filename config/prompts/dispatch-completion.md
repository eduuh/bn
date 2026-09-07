---
name: dispatch-completion
internal: true
description: Standard completion protocol for a dispatched ship lane without a plan-review gate
arguments:
  - { name: workflow_name, description: Selected workflow name., required: true }
---
DELIVERABLE: a PR you validated locally. Before you change anything, REPRODUCE the problem (or
demonstrate the missing behaviour) and record how with `bn_add research` — a failing test, a
command with its wrong output, or a behaviour you observed in the running app. After the change,
prove it against that same reproduction on the PR head: the reproduction now passes and
`bn verify` is green. A green gate alone validates nothing that nobody reproduced.

When the workflow completes green: push, open the PR, run `bn ingest`, then run
`bn agent done --summary '<what you did, and how the reproduction now passes>' --pr <url>`.

The ONLY finish without a PR is an issue that cannot be reproduced. Record every attempt with
`bn_add research`, then run `bn agent done --report-only --summary 'not reproducible: <what you tried>'`.
Anything else that stops you short of a PR — a question only a person can answer, a scope that
turned out wrong — is a blocker, not a finish: `bn_add blocker` (or `bn_add ask`), then
`bn agent done --failed --summary '<what blocks>'`.

Before reporting, record one workflow reflection with
`bn reflect "skill:{{workflow_name}} — <what worked or what to change>"`.
