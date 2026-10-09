# Reference skills

This folder is reserved for **true reference skills** — reusable knowledge or
capability files that are *referenced by name* from prompts (see
[`config/prompts/`](../prompts/)) rather than invoked directly.

Invocable workflows (the slash-command style entries served as MCP prompts by
`bn mcp serve`) live in [`config/prompts/`](../prompts/), structured like
[`config/agents/`](../agents/).

Standards read through `bn ref`, such as writing voice, ship in
[`config/references/`](../references/). Personal references override shipped defaults;
editing a default copies it into the personal store.

Runtime-only launch envelopes live in that same prompt catalog with `internal: true`. Rust loads
them strictly by name, but MCP discovery and native skill installation hide them. This keeps
dispatch, pipeline, critic, attention, curriculum, specialist, and review instructions
Markdown-owned without exposing implementation fragments as user-invocable workflows.

Reference skills are meant to be made available to a harness (Copilot CLI /
Claude Code) natively and then mentioned inside a prompt body, e.g.
"use the `stow` skill". They are not wired into the MCP prompt surface.
