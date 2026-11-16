---
name: pull-ado
description: "Pull assigned Azure DevOps work items into Workspan tasks idempotently. Use to import ADO tasks or sync assigned work items."
when: Starting work from your ADO backlog, or refreshing a repo's todos from assigned sprint items.
keywords: [ado, azure devops, work items, tickets, backlog, sprint, iteration, pull, todos]
arguments:
  - { name: repo, description: "Repository whose task queue receives the items (default: current repo)", required: false }
  - { name: iteration, description: "Explicit ADO iteration path, or a work-item id / sprint name to derive it from (default: all assigned open items)", required: false }
---
You are pulling the user's assigned Azure DevOps work items into Workspan's task queue.
This is PULL-ONLY: read from ADO, write into bn. Never write back to ADO.

The mechanics are scripted. From the target repo's worktree, run:

```bash
~/.bn/config/repo/config/scripts/pull-ado.sh
```

Its default repo is the basename of the git repo you run it from, so outside the target repo
always pass `--repo`. Pass `--repo {{repo}}` if a repo was given, and
`--iteration {{iteration}}` if an iteration was given; otherwise it pulls all assigned open
items. It needs `az` (signed in) and `python3`, and uses your `az devops configure` org/project
defaults unless you pass `--org`/`--project`. It uses `workspan task ingest` with source-scoped
repository/item keys, preserving completed tasks and the selected file/Forge provider.

Then report in 1–3 lines: relay the importer's tally and target repository. If the script fails, report its
relevant error with credentials and private identifiers redacted, then stop; do not fall back to
hand-running `az` queries.

Do not create worktrees, branches, or tmux windows — this skill only refreshes the backlog.
