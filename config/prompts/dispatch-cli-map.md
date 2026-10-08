---
name: dispatch-cli-map
internal: true
description: Shared MCP-to-CLI tool map injected into dispatch envelopes
---
Without bn's MCP tools, use the CLI equivalents: `bn_cat` -> `bn --cat`, `bn_add` ->
`bn add <section> <text>`, `bn_done` -> `bn done <id>`, `bn_goal` -> `bn goal <text>`,
`bn_pr` -> `bn pr <url>`, `bn_context` -> `bn --cat`; any other `bn_*` tool name maps to the matching `bn <subcommand>`
(`bn_ref_append` -> `bn ref append`).
