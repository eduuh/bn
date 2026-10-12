---
name: stow
description: "Close out a session: mark done todos, record unfinished work, decisions and links, sweep for uncaptured knowledge. Use before ending, resetting, compacting or handing off."
when: Before ending, resetting, compacting, or handing off a session.
keywords: [capture links, save url, bookmark, wrap, wrap up, close out, log off, done for the day, stow, save, remember, reset, compact, handoff, session, knowledge, unfinished]
---
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
   - A fact about *this repo* that will still be true next month — how it builds, where a
     subsystem lives, a failure that misleads → also `bn_ref_append` into its reference notes,
     with where it came from and today's date (see `/bn:repo-reference`). `bn learned` is
     dated and personal; the reference note is where the next session on this repo will look.
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

## Quick close-out (was wrap-session)

For a normal end of session with nothing to reset or hand over, do only the note hygiene:

1. Call `bn_cat` to load the branch note and review the open todos.
2. For each todo completed this session, call `bn_done` with its id or text.
3. Record any unfinished work as new todos with `bn_add todo`.
4. Record this session's choices with `bn_add decision` — each as *what you chose and why*, naming
   the alternative you rejected. A decision recorded without its reason is one the next session
   re-litigates from scratch.
5. Record the approaches you tried and abandoned with `bn_add decision` too ("tried X, abandoned
   because Y"). A dead end nobody wrote down is an hour the next session spends again.
6. Record what is still open: unresolved facts with `bn_add research`, questions that need another
   person with `bn_add ask`.
   Anything you learned about *the repo itself* — how it builds, where something lives, a failure
   that misleads — outlives this branch, so put it in the repo's reference notes with
   `bn_ref_append` (with its source and today's date) rather than only in the branch note, which
   disappears when the branch does. See `/bn:repo-reference`.
7. If a PR was opened for this branch, link it with `bn_pr <url>` (or `bn pr <url>`) — the note is
   where the next session and the review queue both look for it, and a summary in chat is not a link.
8. Do not sweep or automatically capture session URLs. Add a link only when it is important,
   repeatedly useful, expensive to rediscover, and fills a gap in a curated category.
9. If the branch's work is merged and finished, run `bn close` in the shell — promptly, since
   the merge→close gap is what `bn metrics` measures as close-out lag.
10. Finish with a 2–3 line summary of what changed and what's left.

Be faithful: only mark todos done that are actually complete.

## Curate links

The link store is a small index of durable entry points, not session history.

1. **Pass the importance gate.** Keep only links that are repeatedly useful, expensive to
   rediscover, and likely to matter months from now. Routine PRs, work items, review comments,
   search results and one-off pages fail this gate.
2. **Find the gap first.** Choose a category from `bn links tags`, then run
   `bn links list <purpose> --tag <category>`. If an existing link serves the need, improve that
   record instead of adding another.
3. **Add deliberately.** Use
   `bn links add <url> --category <category> --name "<what it is>"`, with optional topic tags.
   A link without a category is rejected.
4. **Do not sweep the session.** Stow reports links only when one was intentionally curated.
