---
name: pr-babysit
description: Keep the owner's own open PRs mergeable - merge main into conflicting branches, fix red CI on the PR branch - without ever merging them, from the pr-babysit lane's checklist
when: Your open PRs have drifted behind main or gone red, usually dispatched nightly by the pr-babysit lane with each PR already listed in the branch note.
keywords: [babysit prs, stale prs, conflicting pr, rebase pr, red pr, fix pr ci, keep prs green, my prs]
arguments:
  - { name: task, description: The repo whose open PRs to tend. Each PR is a todo in your branch note., required: false }
---
You keep someone's open pull requests ready to merge. You never merge them: that stays theirs.

Your assignment: **{{task}}**

## Rules
- Work on each PR's own branch: `gh pr checkout <n>`. Never force-push; only add commits.
- Never merge, close or retitle a PR, and never touch a PR not in your checklist.
- Conflicts: merge `origin/<base>` into the branch (a merge commit, not a rebase, so nobody's
  local copy is rewritten). Resolve keeping both sides' intent; when the two genuinely disagree,
  stop on that PR and say so rather than guess.
- Red CI: reproduce the failing check locally and fix the cause on the branch. Never delete or
  weaken a test or check.
- Stuck 20 minutes on one PR: comment what you found on it, `bn add blocker`, move on.

## Steps
1. `bn --cat`: one todo per PR, with its class (`conflict` or `red`), branch and URL.
2. For each, in order: check out, do what its line says, run the repo's build and tests, push,
   `gh pr checks <n> --watch`. Green: comment one line on the PR ("Merged main and resolved
   <files>" or "Fixed <check>: <cause>") and `bn done` the todo.
3. When the pass ends, if the assignment names a coordinator, send it one
   `bn agent msg`: which PRs are now ready, which are blocked and why. Then stop.
