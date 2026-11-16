---
name: pr-babysit
description: "Keep the owner's PRs mergeable without merging them. Use when dispatched with a pr-babysit checklist of conflicts or CI failures."
when: Your open PRs have drifted behind main or gone red and a dispatch packet lists them.
keywords: [babysit prs, stale prs, conflicting pr, rebase pr, red pr, fix pr ci, keep prs green, my prs]
arguments:
  - { name: task, description: The repo and PR checklist., required: false }
---
You keep someone's open pull requests ready to merge. You never merge them: that stays theirs.

Your assignment: **{{task}}**

## Rules
- Use a clean, exclusively owned worktree for each checklist PR. Confirm branch ownership
  before checkout or merge; stop on dirty, shared, or uncertain state. Load `commit-privacy`
  before creating or publishing commits. Never force-push; only add authorized commits.
- Never merge, close or retitle a PR, and never touch a PR not in your checklist.
- Conflicts: merge `origin/<base>` into the branch (a merge commit, not a rebase, so nobody's
  local copy is rewritten). Resolve keeping both sides' intent; when the two genuinely disagree,
  stop on that PR and say so rather than guess.
- Red CI: reproduce the failing check locally and fix the cause on the branch. Never delete or
  weaken a test or check.
- Stuck 20 minutes on one PR: comment the concrete blocker on it, update the task, and move on.

## Steps
1. Read the dispatch packet: one checklist entry per PR, with class (`conflict` or `red`), branch and URL.
2. For each, in order: check out, do what its line says, run the repo's build and tests, push,
   `gh pr checks <n> --watch`. Green: comment one line on the PR ("Merged main and resolved
   <files>" or "Fixed <check>: <cause>") and update the assigned task's checklist.
3. When the pass ends, if the assignment names a coordinator, send it one
   `bn agent msg`: which PRs are now ready, which are blocked and why. Then stop.
