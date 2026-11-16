---
name: start-session
description: "Orient on the current task, worktree, and captured session history. Use first when starting, resuming or picking up work."
when: Beginning a work session or picking up a branch — first thing, before you start coding.
keywords: [start-session, status review, task, entire, start, begin, session, kick off, pick up, resume, get going, focus]
arguments:
  - { name: focus, description: Optional area to focus this session on, required: false }
---
1. Run `workspan discover` with the requested focus ({{focus}}), then inspect
   `git status --short --branch` and `workspan task --repo . --json`.
2. Read the assigned task's context at the path printed by `workspan task doc <id>`.
   Do not claim or create a task merely to orient. For an existing plan, use
   `workspan plan status` and read the reported active file.
3. Use the resumed conversation or Entire's captured session history for prior reasoning.
   Do not reconstruct a parallel session log. If needed history is unavailable, state the gap.
4. Summarize the goal, blockers, and first concrete action in 1-2 lines. Prioritize the supplied
   focus; otherwise use the assigned task. Ask before selecting unrelated work.
5. Keep actionable state on the task and acceptance checklist as work happens. Mark a task done
   with `workspan task do <id>` only when its criteria are met; whole-session history belongs to
   Entire, not to the task document.

For cross-project status use `workspan task --all`, `workspan agent live`, and `workspan next`.
For the day's priorities use `morning`.
