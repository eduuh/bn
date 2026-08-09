---
name: dispatch-completion
internal: true
description: Standard completion protocol for a dispatched ship lane without a plan-review gate
arguments:
  - { name: workflow_name, description: Selected workflow name., required: true }
---
When the workflow completes green, open a PR, run `bn ingest`, then run
`bn agent done --summary '<one line of what you did>' --pr <url>`. Before reporting, record one
workflow reflection with `bn reflect "skill:{{workflow_name}} — <what worked or what to change>"`.
