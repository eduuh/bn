<!-- BEGIN bn (managed by `bn mcp init`) -->

## Workspan agent integration

Workspan coordinates workspaces, tasks, agents, machines, and execution. Entire owns captured
agent-session history and checkpoints. The `bn` executable and retained MCP names remain
compatibility interfaces.

Start with `workspan discover <task>`. Read actionable work with `workspan task --repo .` and the
assigned task's context at the path printed by `workspan task doc <id>`. Use
`workspan plan status` for an existing plan and `workspan agent live` for live agents.
Do not create a task or plan merely to initialize a worktree.

Keep acceptance progress, blockers, and necessary decisions on the governing task/provider.
Mark work done with `workspan task do <id>` only when its criteria are met. Preserve file/Forge
provider authority; do not silently substitute a local store when a remote provider fails.
Do not create a second session transcript, memory ledger, or handoff packet.

The client launches `workspan mcp serve` over stdio in its worktree. Retained MCP capabilities
serve commands, curated links, references, and workflow prompts; inspect their actual schemas.
Use the CLI for tasks, execution, machines, and fleet operations. No session-note tools are
needed. Existing historical files are archives, not an execution prerequisite.

Reusable workflows live in `config/prompts/`. Claude serves them as MCP prompts; Copilot installs
native skills. Edit the Workspan source, not generated skill copies.

<!-- END bn -->
