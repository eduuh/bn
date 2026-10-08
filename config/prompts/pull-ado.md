---
name: pull-ado
description: "Pull assigned Azure DevOps work items into the repo main note as idempotent todos. Use to import my ADO tasks or sync work items."
when: Starting work from your ADO backlog, or refreshing a repo's todos from assigned sprint items.
keywords: [ado, azure devops, work items, tickets, backlog, sprint, iteration, pull, todos]
arguments:
  - { name: repo, description: "bn repo whose main note receives the todos (default: current repo)", required: false }
  - { name: iteration, description: "Explicit ADO iteration path, or a work-item id / sprint name to derive it from (default: all assigned open items)", required: false }
---
You are pulling the user's assigned Azure DevOps work items into a branch note's backlog.
This is PULL-ONLY: read from ADO, write into bn. Never write back to ADO.

The mechanics are scripted. From the target repo's worktree, run:

```bash
~/.bn/config/repo/config/scripts/pull-ado.sh
```

Its default repo is the basename of the git repo you run it from, so outside the target repo
always pass `--repo`. Pass `--repo {{repo}}` if a repo was given, and
`--iteration {{iteration}}` if an iteration was given; otherwise it pulls all assigned open
items. It needs `az` (signed in) and `python3`, and uses your `az devops configure` org/project
defaults unless you pass `--org`/`--project`. It writes each item into the repo's `main` note as an idempotent `[#<id>] <title>` todo
(`--if-absent "[#<id>]"` dedups re-pulls and keeps completed todos from being resurrected).

Then report in 1–3 lines: relay the script's tally — items pulled, new vs. already-present,
the target note (`<repo>/main`) — and list the new ids. If the script fails, report its
stderr verbatim and stop; do not fall back to hand-running `az` queries.

Do not create worktrees, branches, or tmux windows — this skill only refreshes the backlog.
