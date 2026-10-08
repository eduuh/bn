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
8. Capture the links this session leaves behind: `bn links capture` stores the branch's PR and
   work item, and `bn links add <url> --name "<what it is>" --tags <tags>` takes anything else
   you will want again — the PBI, an Azure portal blade, a dashboard, the doc that answered the
   question. They are person-scoped and cross-machine, so they outlive this worktree; a URL only
   in the note leaves when the worktree does.
9. If the branch's work is merged and finished, run `bn close` in the shell — promptly, since
   the merge→close gap is what `bn metrics` measures as close-out lag. It warns about any PR or
   work item you have not captured.
10. Finish with a 2–3 line summary of what changed and what's left.

Be faithful: only mark todos done that are actually complete.

## Capture links (was capture-links)

You are capturing the links from this session that are worth going back to.

A link nobody stores is a link somebody re-finds. The branch note holds exactly two of them
(`pr:` and `work_item:`) and both leave when the worktree does, so anything you want next month
belongs in the `bn links` store — person-scoped, cross-machine, and ranked by how often it is
actually opened.

1. **Sweep the session.** Read back over what you and the user touched and list every URL that
   would cost real time to find again: pull requests, work items and PBIs, Azure portal blades,
   dashboards and queries, internal docs and wikis, API references, the one Stack Overflow answer
   that actually worked. Skip anything trivially re-findable — a package's front page, a search
   result, a link the user will never open twice.

2. **Capture the branch's own links first.** If this branch has a PR or a work item, run
   `bn links capture` in the shell — it stores both, named and tagged with the repo, in one
   command. Do this the moment a pull request completes, not at the end of the day: the gap
   between "merged" and "worktree gone" is where these links get lost.

3. **Store the rest**, one call each, with the `bn_links_add` MCP tool (or `bn links add <url>`
   in the shell). Give every link:
   - a `name` you would actually search for — what it *is*, not what its `<title>` says. "Prod
     subscription access request" beats "Microsoft Azure".
   - `tags` from the configured vocabulary (`bn links tags` prints it, with counts). A link takes
     as many as apply. An unlisted tag is kept, not rejected, so reach for a new one when nothing
     fits — but check the set first, because six tags you use beat twenty you don't. Omit `tags`
     and they are guessed from the host: an ICM incident gets `icm`, learn.microsoft.com gets
     `docs`, myaccess gets `permissions`, security.microsoft.com gets `security`.
   - the `repo`, when the link belongs to one. Those sort first when you are in that repo.

4. **Don't fetch anything.** Names are written, never scraped. If you cannot say what a link is
   for, ask the user rather than storing a URL nobody will recognise in three months.

5. **Report** in one line: how many links you stored, and name any you deliberately skipped so
   the user can overrule you.

Be honest about duplicates: re-adding a URL already stored updates it in place, which is the right
move when you are improving a name or fixing tags, and pointless noise otherwise.
