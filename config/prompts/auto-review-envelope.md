---
name: auto-review-envelope
internal: true
description: Runtime envelope around the public auto-review-pr workflow
arguments:
  - { name: workflow_body, description: Strictly rendered auto-review-pr workflow., required: true }
---
You are in a fresh, sparse worktree for a pull request dispatched for review. Follow the
`auto-review-pr` workflow below as the complete process. Without bn's MCP tools, use `bn --cat` for
`bn_cat` and `bn add <section> <text>` for `bn_add`.

--- WORKFLOW: auto-review-pr ---
{{workflow_body}}
--- END WORKFLOW ---
