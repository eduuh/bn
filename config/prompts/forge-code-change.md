---
name: forge-code-change
description: Complete a Forge-assigned repository change with reproduction, implementation, validation, PR, ingest, and Forge finish update.
when: A Forge-assigned task requires editing code, docs, config, tests, or repo-owned skill files.
keywords: [forge, code change, implementation, PR, reproduce, validate, bn ingest, finish_activity, agent PR]
arguments:
  - { name: task, description: Code-change goal from Forge., required: false }
---
Use this lane when the Forge task requires changing repository files.

1. Read `bn --cat`, then reproduce or confirm the requested behavior before editing. Record the
   repro or confirmation with `bn_add research`.
2. Make the smallest change that satisfies the Forge task ({{task}}). Do not fix adjacent issues.
3. Add or update the narrowest test or render check that would catch the failure or prove the new
   behavior.
4. Validate with the smallest relevant command first, then `bn verify` before finalizing.
5. Stage only files changed for this task. Commit with a conventional message, push, open a PR, and
   record it with `bn_pr`.
6. Run `bn ingest` from the worktree. If this is a pooled checkout, it must still ingest from the
   current matching checkout.
7. Finish Forge with a rich Markdown report. Use headings, checklists, tables, links, screenshots,
   or diagrams when they make the change easier to review. Include:
   - what changed
   - validation run
   - PR URL
   - blockers or follow-up, if any

If the change cannot be safely made, finish Forge as `blocked` with the exact missing permission,
data, or decision.
