<!-- BEGIN bn (managed by `bn mcp init`) -->

## Workspan agent integration

Workspan coordinates workspaces, agents, machines, and execution. Entire handles captured
agent-session history. The `bn` command and MCP tool names remain compatibility interfaces.

### Tasks, execution, and references

The configured file or Forge task provider owns actionable work. Use `workspan task` to inspect
tasks, `workspan task doc <id>` for context and acceptance criteria, and `workspan task do <id>`
when the task is actually complete. Task documents survive changes to the execution branch.
Use `workspan plan status` / `workspan plan path` for the execution contract and
`workspan pipeline status` / `workspan pipeline set <stage>` for execution state.
Entire owns captured session history; do not create a parallel session-memory store.

The per-worktree stdio launcher (`bn mcp serve`) exposes only:

- `bn_cmd` — list or run commands in the current workspace.
- `bn_links_search` / `bn_links_add` — curated durable links.
- `bn_ref` / `bn_ref_append` — reusable repository references and standards.

Use the Workspan CLI for tasks, execution, attention, scheduling, and fleet operations.
MCP workflow prompts remain available from the effective skills.

Fleet orchestration is driven natively from the `bn` CLI — no tmux dashboard required. Drive and
monitor agents across repos with `bn orchestrate` (`status`, `supervise [--all-repos]`,
`scale`, `add`, `--queue`, `ask` / `answers`, `backlog --all-repos`) alongside `bn next` and
`bn doctor`.

Record task completion and execution transitions when they actually happen, not batched at
session end. Record only what genuinely occurred.

Prompts are reusable workflows in `config/prompts/`. In Claude Code they are served as MCP prompts
(`/bn:<name>`); in Copilot CLI they are installed as native skills referred to by their name (not
`/mcp.bn.<name>` — Copilot does not surface MCP prompts). Every effective skill is served, so a
prompt exists for each workflow the tiers define; use the CLI for cross-branch workflows, and note
that some prompts (PR review, orchestration) describe steps whose commands live only in the CLI.
Reference skills sit in `config/skills/` and are referred to by name from a prompt's body
rather than invoked directly.

<!-- END bn -->
