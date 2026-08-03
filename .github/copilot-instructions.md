<!-- BEGIN bn (managed by `bn mcp init`) -->

## Branch notes (bn)

This repo's branch-note tools are served by a per-worktree stdio launcher (`bn mcp serve`) that
your MCP client spawns automatically. They are scoped to the current worktree's branch note — the
launcher derives its (repo, branch) from your working directory, so nothing needs to be running
first. Keep this branch's note current as you work — don't wait to be asked:

- `bn_context` — compact session-start orientation (goal, counts, top todos, blockers, PR,
  next-step hints) in one call. Call it first.
- `bn_goal` — set the one-line goal first thing in a fresh worktree.
- `bn_add` — record a todo / blocker / decision / research / collab / ask as it comes up.
- `bn_done` — close a todo the moment it's finished.
- `bn_pr` — record the PR url once opened.
- `bn_cat` / `bn_status` — read only the current branch note.
- `bn_sync` / `bn_handoff_read` / `bn_handoff_write` — reconcile or hand off only this branch.

The MCP surface intentionally cannot target another repo or branch and does not expose
cross-branch rollups, search, daily goals, reviews, metrics, attention queues, cron status, or
agent fleet/session mining. Use the `bn` CLI explicitly for those broader workflows.

Timing matters: every mutation is timestamped as it happens, so mark `bn_done` when the work
actually happens — not batched at session end — and only record what genuinely occurred.

Skills are reusable workflows served as MCP prompts (`/bn:<name>` in Claude Code,
`/mcp.bn.<name>` in Copilot). MCP only serves session-safe skills such as `start-session`,
`wrap-session`, `stow`, `axi`, and `lavish-plan`; use the CLI for cross-branch skills.

<!-- END bn -->
