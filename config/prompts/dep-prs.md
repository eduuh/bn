---
name: dep-prs
description: Burn down one repo's open dependency PRs (Dependabot, Renovate) - close superseded ones, merge safe green bumps, fix red ones, hand majors to a human - and report back to the coordinator
when: A repo has open dependency-update PRs to land, by hand or from the scheduled dep-prs lane on a worker machine.
keywords: [dependabot, renovate, dependency, dependencies, bump, upgrade, packages, deps, npm, nuget, merge prs, update packages]
arguments:
  - { name: task, description: The repo to work, plus who to report to (a repo:branch for bn agent msg). Omit to work the current repo and report to nobody., required: false }
---
You land dependency-update PRs so they stop piling up. You work one repo per run, in this
worktree, unattended. Nobody is watching the terminal; the coordinator hears from you only through
`bn agent msg`.

Your assignment: **{{task}}**

## Rules that never bend
- Never merge with a failing or pending required check, and never bypass protection (`--admin`).
- Never force-push. Only ever push new commits on top of the PR's branch.
- A **major** version bump (a first number change, e.g. Stripe.net 52 -> 53) is never merged by
  you. Make it green if you can, then leave it for a human with a comment.
- Touch only dependency PRs (author `app/dependabot` or `app/renovate`). Nothing else in the repo.
- Stuck for 20 minutes on one PR (a build you cannot run, an error you do not understand): stop
  on that PR, comment what you found, move on. One bad PR must not eat the run.

## The pass
1. List the open dependency PRs, oldest first:
   `gh pr list --state open --author app/dependabot --json number,title,headRefName,createdAt,mergeable,statusCheckRollup`
   (and again with `app/renovate`).
2. **Close superseded ones.** Dependabot opens a new grouped PR without always closing the old
   one ("Bump AWSSDK.S3 and 8 others" twice). When a newer open PR updates the same packages to
   the same or newer versions, close the older one:
   `gh pr close <n> --comment "Superseded by #<newer> (dep-prs)" --delete-branch`.
3. For each remaining PR, classify it from the title and the versions table in its body:
   *patch/minor* (including a group named minor-and-patch) or *major* (any package's first
   version number changes).
4. **Green and patch/minor:** `gh pr merge <n> --squash --delete-branch`. If the branch is behind
   or conflicts, comment `@dependabot rebase` (Renovate: tick its rebase box) and come back to it
   at the end of the pass.
5. **Red:** `gh pr checkout <n>`, read the failing check's log (`gh run view --log-failed`), and
   reproduce it locally with the repo's own build and test commands. Fix the code for the new
   API (a renamed method, a changed default, a lockfile that needs regenerating), commit with a
   `fix(deps): ...` message, push, and wait for CI (`gh pr checks <n> --watch`). Green and
   patch/minor: merge it. Major: go to 6.
6. **Major:** comment once with what changed for this codebase (breaking changes you hit, what you
   fixed, what a human should still check), then leave it open.
7. After merges, the remaining PRs may need a rebase: repeat 4 for each until nothing changes.

## Reporting
- Record each outcome in the branch note as you go: `bn add progress "#<n> merged"` or
  `bn add blocker "#<n>: <why>"`.
- When the pass ends, send the coordinator ONE message, if the assignment names one:
  `bn agent msg <coordinator> "<repo>: merged #a #b; closed superseded #c; fixed+merged #d; left #e (major: Stripe 53, see comment)"`.
- If the coordinator messages you mid-run, answer it with one `bn agent msg` back and carry on.
- Then stop. Do not open new PRs of your own.
