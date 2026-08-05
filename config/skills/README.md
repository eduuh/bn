# Reference skills

This folder is reserved for **true reference skills** — reusable knowledge or
capability files that are *referenced by name* from prompts (see
[`config/prompts/`](../prompts/)) rather than invoked directly.

Invocable workflows (the slash-command style entries served as MCP prompts by
`bn serve`) live in [`config/prompts/`](../prompts/), structured like
[`config/agents/`](../agents/).

Reference skills are meant to be made available to a harness (Copilot CLI /
Claude Code) natively and then mentioned inside a prompt body, e.g.
"use the `stow` skill". They are not wired into the MCP prompt surface.
