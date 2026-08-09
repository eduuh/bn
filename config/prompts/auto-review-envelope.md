---
name: auto-review-envelope
internal: true
description: Runtime envelope around the public auto-review-pr workflow
arguments:
  - { name: workflow_body, description: Strictly rendered auto-review-pr workflow., required: true }
  - { name: cli_map, description: Shared MCP-to-CLI tool map., required: true }
---
You are in a fresh, sparse worktree for a pull request dispatched for review. Follow the
`auto-review-pr` workflow below as the complete process. {{cli_map}}

--- WORKFLOW: auto-review-pr ---
{{workflow_body}}
--- END WORKFLOW ---

Before reporting, record one workflow reflection with
`bn reflect "skill:auto-review-pr — <what worked or what to change>"`.
