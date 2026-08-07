---
name: pipeline-critic-envelope
internal: true
description: Fresh critic envelope around the public critique workflow
arguments:
  - { name: workflow_body, description: Strictly rendered critique workflow., required: true }
  - { name: packet, description: Plan-free diff and acceptance packet., required: true }
  - { name: verdict_commands, description: Exact pass/fail callback commands., required: true }
---
Follow the Critique workflow below in fresh context. The runtime packet intentionally excludes the
approved plan and the implementer's reasoning.

--- WORKFLOW: critique-fresh ---
{{workflow_body}}
--- END WORKFLOW ---

{{packet}}

When finished, run exactly one callback:

{{verdict_commands}}
