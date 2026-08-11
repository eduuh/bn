---
name: stow
description: Sweep the current session for uncaptured durable knowledge and unfinished work before reset or handoff
when: Before ending, resetting, compacting, or handing off a session.
keywords: [stow, save, remember, reset, compact, handoff, session, knowledge, unfinished]
---
You are stowing the current session so it is safe to reset, compact, or hand off.

1. **Load what is already durable.** Call `bn_cat`, then inspect the current branch's uncovered
   agent-session proposals with `bn agent digest --json`. Treat both as evidence, not as a reason
   to duplicate entries.

2. **Sweep the conversation.** Identify durable facts that still exist only in this session:
   - codebase, build, test, tooling, or operational facts;
   - workflow lessons or improvements for a future session;
   - branch-scoped decisions, each with its rationale and the alternative it beat;
   - approaches tried and abandoned, with the reason they failed;
   - unresolved factual questions or questions requiring another person;
   - unfinished actions and concrete next steps;
   - a PR opened for this branch that the note does not already link.
   Ignore transient narration, guesses, secrets, and facts already captured accurately.

3. **Curate before writing.** For every finding, compare it with the note and digest proposals.
   Skip exact duplicates. If a new fact supersedes an existing entry, record the corrected,
   self-contained version rather than preserving both claims. Do not mark work complete unless it
   actually is.

4. **Route only through existing bn surfaces.**
   - Durable codebase/tooling/operational facts → `bn learned "<fact>"`.
   - A reusable improvement to how the work or a skill should run → `bn reflect "<improvement>"`.
   - A branch-scoped choice that should outlive the session → `bn_add decision`, carrying the *why*
     and the alternative rejected. Stripped of its reason it will simply be argued again.
   - An approach that failed and should not be retried → `bn_add decision` too ("tried X, abandoned
     because Y"). A dead end is durable knowledge; only the narration around it is transient.
   - An unresolved fact to investigate → `bn_add research`.
   - A question requiring input from another person → `bn_add ask`.
   - An unfinished action with a concrete outcome → `bn_add todo`.
   - A PR the note does not yet link → `bn_pr <url>`, so a cold-start session can reach the work
     without re-deriving where it went.
   - Accurate uncovered session telemetry → apply it with `bn agent digest`; use `--yes` only when
     every proposal is accurate and non-duplicative.

   Do not create or edit another memory file, skill, project instruction file, or external issue.
   Stow routes knowledge into bn; it does not invent a parallel store or publish externally.

5. **Leave a cold-start packet.** Call `bn_handoff_write` after the writes, using the remaining
   open todos as the priority queue and including any deferred decisions or open questions. Write
   it so a fresh session can identify the goal, current state, and next action without this
   conversation.

6. **Report honestly.** State what was stowed and which bn surfaces received it, what remains
   uncaptured (and why), and whether the session is safe to reset. It is safe only when every
   durable finding and unfinished next step from this sweep is represented in bn and the handoff
   packet is readable.

Without the bn MCP tools, use the CLI equivalents (`bn_cat` → `bn --cat`; any other `bn_*` tool
maps to the matching `bn <subcommand>`, e.g. `bn_handoff_write` → `bn handoff write`).
