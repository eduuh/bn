---
name: pipeline-plan-envelope
internal: true
description: Fresh plan-stage envelope around the public plan workflow
arguments:
  - { name: workflow_body, description: Strictly rendered plan workflow., required: true }
  - { name: packet, description: Deterministic task and investigation packet., required: true }
  - { name: plan_file, description: Absolute path of the bn-owned draft plan file., required: true }
---
Work in fresh context and follow the Plan workflow below. Do not write product code.

--- WORKFLOW: plan ---
{{workflow_body}}
--- END WORKFLOW ---

{{packet}}

## Plan artifact

Write the plan Markdown directly to this Workspan-owned draft file, not to a duplicate task
items:

```text
{{plan_file}}
```

Keep actionable progress on the governing task; Entire owns session history.

Before finishing, record one workflow reflection with
`bn reflect "skill:plan — <what worked or what to change>"`.
