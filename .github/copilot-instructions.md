<!-- BEGIN bn (managed by `bn mcp init`) -->

## Branch notes (bn)

This repo's branch-note tools are served by a per-worktree stdio launcher (`bn mcp serve`) that
your MCP client spawns automatically. They are scoped to the current worktree's branch note — the
launcher derives its (repo, branch) from your working directory, so nothing needs to be running
first. Keep this branch's note current as you work — don't wait to be asked:

- `bn_context` — compact session-start orientation (goal, counts, top todos, blockers, PR,
  next-step hints) in one call. Call it first.
- `bn_add` — record a todo / blocker / decision / research / collab / ask as it comes up.
- `bn_done` — close a todo the moment it's finished.
- `bn_cat` — read only the current branch note.

This is a deliberately slim tool surface for efficiency. The single-purpose tools (`bn_goal`,
`bn_pr`, `bn_status`, `bn_sync`, `bn_handoff_read`, `bn_handoff_write`) are not advertised over
MCP — use the `bn` CLI (`bn goal`, `bn pr`, `bn status`, `bn sync`, `bn handoff …`) when needed.
If the MCP server is unavailable, keep working through the CLI instead of blocking: each `bn_*`
tool maps to the same words after `bn` (`bn_context` → `bn status`, `bn_cat` → `bn cat`,
`bn_add` → `bn add`, and `bn_done` → `bn done`).

The MCP surface intentionally cannot target another repo or branch and does not expose
cross-branch rollups, search, daily goals, reviews, metrics, attention queues, cron status, or
agent fleet/session mining. Use the `bn` CLI explicitly for those broader workflows.

Fleet orchestration is driven natively from the `bn` CLI — no tmux dashboard required. Drive and
monitor agents across repos with `bn orchestrate` (`status`, `supervise [--all-repos]`,
`scale`, `add`, `--queue`, `ask` / `answers`, `backlog --all-repos`) alongside `bn next` and
`bn doctor`.

Timing matters: every mutation is timestamped as it happens, so mark `bn_done` when the work
actually happens — not batched at session end — and only record what genuinely occurred.

Prompts are reusable workflows in `config/prompts/`. In Claude Code they are served as MCP prompts
(`/bn:<name>`); in Copilot CLI they are installed as native skills referred to by their name (not
`/mcp.bn.<name>` — Copilot does not surface MCP prompts). Every effective skill is served, so a
prompt exists for each workflow the tiers define; use the CLI for cross-branch workflows, and note
that some prompts (PR review, orchestration) describe steps whose commands live only in the CLI.
Reference skills sit in `config/skills/` and are referred to by name from a prompt's body
rather than invoked directly.

<!-- END bn -->
