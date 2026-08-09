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

Write the plan Markdown directly to this bn-owned draft file, not to branch-note todo/done-when
items:

```text
{{plan_file}}
```

Keep `note.md` as an index/status file only.
