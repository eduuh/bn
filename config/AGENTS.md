# Agent instructions (global, harness-neutral)

Shipped with bn. Claude Code reads this through an `@` import in `~/.claude/CLAUDE.md`; Copilot CLI reads it as `~/.copilot/copilot-instructions.md`. Keep it short: every line here is loaded into every session.

## Be lean
- Answer first, then only the detail the reader needs. No preamble, no recap of what you were asked.
- Short sentences. No em-dashes. No headers in short replies.
- Do not paste long command output into chat. Summarise it and name the file or command.
- Do only what was asked. Mention adjacent problems in one line instead of fixing them.
- Prefer one targeted tool call over a sweep. Read the part of a file you need, not the whole file.

## Track work with bn
- Read the branch note at session start (`bn --cat`). Set the goal if it is empty: `bn goal "..."`.
- Record as you go, never batched: `bn add todo|decision|blocker|research|ask "..."`, and `bn done <id>` the moment a todo is finished.
- Repo knowledge that will still be true next month goes in the reference notes: `bn ref --repo` to read the index, `bn ref append <name> --repo --section <s> "<fact> (src, date)"` to add one fact.
- Find a workflow before improvising: `bn skills find "<what you are about to do>"`, then invoke it (`/bn:<name>` in Claude Code, `/<name>` in Copilot CLI).

## Reach for these skills
- `grill-me` before committing to a plan with open branches.
- `verification-before-completion` before saying anything is done, fixed, or passing.
- `resolving-merge-conflicts` for an in-progress merge or rebase conflict.
- `handoff` or `/bn:stow` before a reset, compact, or hand-over.
- `playwright-cli` to drive a browser; `worktree-sweep` for stale worktrees and tmux.

## Git
- Commit each logical unit with a conventional message (`feat:`, `fix:`, `chore:`).
- Never bare `git stash` / `git stash pop`; the stash is shared across worktrees.
- Never force-push, reset --hard, or delete branches without being asked.
