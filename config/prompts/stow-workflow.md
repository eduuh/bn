---
name: stow-workflow
internal: true
description: "Close out active work without duplicating session history. Use before ending, resetting, compacting or handing off."
when: Before ending, resetting, compacting, or handing off a session.
keywords: [wrap, close out, stow, reset, compact, handoff, unfinished, knowledge]
---
1. **Check the work, not a session ledger.** Inspect the diff, the assigned task, and any active
   plan. Compare completed work with its acceptance criteria. Entire owns the conversation and
   reasoning history; do not copy it into another handoff or memory file.
2. **Update actionable state.** Check completed plan steps immediately. Mark the assigned task
   done with `workspan task do <id>` only when its outcome is verified. Put remaining actions,
   concrete blockers, decisions needed to proceed, and the next step in its existing context
   document (`workspan task doc <id>` prints the path). For Forge-owned work, update the activity
   through its provider. Do not silently substitute a local file for an unavailable provider.
3. **Keep durable knowledge separate.** A verified contributor-wide fact belongs in the repo
   references with its source and date (`repo-reference`). A reusable workflow improvement goes
   through `workspan reflect`. Skip duplicates, guesses, secrets, and transient narration.
   Do not create skills, instruction files, or external issues without authorization.
4. **Check capture before a reset.** Run `entire status` and report any capture failure or missing
   history honestly. A configured capture agent alone is not proof that this session was saved.
   Do not enable capture, change its private destination, or upload history as a close-out side
   effect. If the session is not safely captured, do not claim it is safe to reset.
5. **Hand back the outcome.** State what changed, where unfinished work is recorded, and any
   capture gap. Link a PR through the task's existing context/provider, not a bookmark store.
   Do not create a second session transcript or serialized handoff packet.

## Curate links

The link store holds repeatedly useful entry points, not session history. Routine PRs, work items,
review comments, and one-off pages do not belong.

Before adding an important link, select a category from `workspan links tags` and search
`workspan links list <purpose> --tag <category>`. Improve an existing entry when it already serves
the need. Otherwise use
`workspan links add <url> --category <category> --name "<what it is>"`.
