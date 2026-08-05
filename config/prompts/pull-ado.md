---
name: pull-ado
description: Pull assigned Azure DevOps work items into a repo's main note as idempotent [#id] todos
when: Starting work from your ADO backlog, or refreshing a repo's todos from assigned sprint items.
keywords: [ado, azure devops, work items, tickets, backlog, sprint, iteration, pull, todos]
arguments:
  - { name: repo, description: "bn repo whose main note receives the todos (default: current repo)", required: false }
  - { name: iteration, description: "Explicit ADO iteration path, or a work-item id / sprint name to derive it from (default: all assigned open items)", required: false }
---
You are pulling the user's assigned Azure DevOps work items into a branch note's backlog.
This is PULL-ONLY: read from ADO, write into bn. Never write back to ADO.

1. **Target note.** The destination is the `main` note of `{{repo}}` (the repo's backlog).
   If `{{repo}}` is empty, call `bn_status` to learn the current repo and use that. All
   writes go to `repo=<that repo>`, `branch=main`.

2. **Query ADO for assigned, open work items.** Use the `az` CLI via Bash:
   - If `{{iteration}}` is an explicit iteration path, filter on it. If it's a numeric id or
     ADO URL, first read that item's `System.IterationPath` (`az boards work-item show
     --id <id> -o json`, parse with `json.loads(..., strict=False)`) and filter on that. If
     empty, query all assigned open items.
   - ```bash
     az boards query --wiql "SELECT [System.Id],[System.Title],[System.State],[System.WorkItemType] FROM WorkItems WHERE [System.AssignedTo] = @Me AND [System.State] NOT IN ('Closed','Resolved','Removed','Done')<ITERATION> ORDER BY [System.State]" -o json
     ```
     where `<ITERATION>` is ` AND [System.IterationPath] = '<path>'` when one was resolved,
     else empty. Parse with `strict=False` (ADO JSON may carry control characters).

3. **Write each item as an idempotent todo.** For every returned item, call `bn_add` with:
   - `section: "todo"`, `repo: <target repo>`, `branch: "main"`
   - `text: "[#<id>] <title>"`
   - `if_absent: "[#<id>]"`  ← the dedup key; a second pull of the same item is skipped,
     even if its title changed or the todo was already completed.

   The CLI equivalent (if you prefer Bash) is:
   ```bash
   bn add --repo <repo> --branch main todo "[#<id>] <title>" --if-absent "[#<id>]"
   ```

4. **Report.** Summarize in 1–3 lines: how many items were pulled, how many were new vs.
   already-present (skipped), and the target note (`<repo>/main`). List the new ids.

Do not create worktrees, branches, or tmux windows — that's `sprint-setup`'s job. This
skill only refreshes the backlog. Closed/Resolved/Removed items are filtered out by the
query, and `if_absent` keeps completed pulls from being resurrected.
