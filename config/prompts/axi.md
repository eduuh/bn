---
name: axi
description: Run an AXI-style autonomous work loop on the current branch — orient, execute one todo at a time, record as you go, report compactly
when: An agent is asked to work autonomously on this branch, or you want AXI-style (agent-experience) discipline — one-call orientation, content-first execution, honest in-the-moment tracking.
keywords: [axi, agent, autonomous, loop, execute, work loop, agent experience, orientation, next todo]
arguments:
  - { name: focus, description: Optional area or todo to focus the loop on, required: false }
---
You are running an AXI-style work loop on the current branch. AXI (https://axi.md/) favors
compact orientation, content-first steps, and definitive reporting — no ceremony.

1. **Orient in one call**: `bn_context` — goal, open todo/blocker counts, top todos, PR, and
   next-step hints. If `handoff_available` is true, follow with `bn_handoff_read` for the full
   orientation. If `goal` is null, set it now with `bn_goal` before anything else.
2. **Pick one item**: the first open todo (or the one matching {{focus}}). If a blocker covers
   it, surface the blocker instead of working around it silently.
3. **Execute content-first**: do the work with the repo's own tools (build, test, editor).
   New work discovered on the way → `bn_add section=todo` immediately, don't hold it in memory.
   Choices with rationale → `bn_add section=decision`.
4. **Close honestly**: `bn_done <id>` the moment the item actually lands — timestamps feed the
   metrics, so never batch closes at the end. Partial progress → `bn_done <id> pct=NN`.
5. **Loop**: return to step 2 while open todos remain and nothing blocks; stop when the list is
   empty, a blocker needs a human, or the session goal is met.

Final report — keep it to this shape:
- **Done**: items closed this loop (ids + one line each).
- **Open**: what remains, in priority order.
- **Blocked**: blockers needing a human, if any.
- **Next**: the single next step for whoever picks this up.
