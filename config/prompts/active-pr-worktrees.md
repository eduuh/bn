---
name: active-pr-worktrees
description: Discover or materialize worktrees for your active GitHub and Azure DevOps PRs, including PRs created on another machine
when: You want to resume your own open PR work on this machine, list remote-only PR branches, or create their local worktrees.
keywords: [active prs, my prs, github prs, ado prs, azure devops prs, remote prs, worktrees, other laptop, resume pr, fetch pr]
arguments:
  - { name: target, description: "Optional PR number, #number, or source branch to materialize. Omit to list remote-only PRs.", required: false }
  - { name: all, description: "If true/yes/1, materialize every active PR missing a local worktree.", required: false }
  - { name: no_tmux, description: "If true/yes/1, create worktrees without opening tmux windows.", required: false }
---
You are finding the user's own active pull requests for the repository containing the current
worktree and, when requested, materializing them locally. The PR may have been created from another
laptop. GitHub and Azure DevOps are both supported; `bn wt remote` detects the provider from
`origin`, queries the authenticated identity, favourites limited ADO refs when necessary, fetches
the exact source branch, and creates an idempotent worktree.

Use the harness to run exactly one `bn wt remote` command from the current repository:

1. Interpret the arguments:
   - If `{{target}}` is non-empty, run `bn wt remote {{target}}`.
   - Else if `{{all}}` is truthy (`true`/`yes`/`1`), run `bn wt remote --all`.
   - Otherwise run `bn wt remote` to list active PRs that do not yet have a local worktree. Harness
     shell calls are non-interactive, so this prints the candidates instead of opening fzf.
   - Add `--no-tmux` when `{{no_tmux}}` is truthy.
   - `target` wins over `all`; never run both forms.

2. Do not reproduce the provider logic with hand-written `gh`, `az`, or `git worktree` commands.
   Do not query PRs from other authors. Do not create a new branch when the requested remote PR
   cannot be found or fetched; report the command's error instead.

3. Report the result compactly:
   - List mode: show each PR number, title, branch, and URL emitted by the command.
   - Target mode: report the PR number/branch and resulting worktree path.
   - All mode: report each created or already-existing worktree and the final tally.
   - On failure, preserve the actionable authentication/provider error and stop.

This skill is repository-scoped per invocation. To process another repository, run it from any
worktree of that repository. For an interactive cross-repository view, use `bn tat`; its
per-repository picker shows the same remote-only PRs beside local worktrees.
