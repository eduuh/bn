---
name: wrap-session
description: Wrap up the current session — mark done todos, record unfinished work & decisions
when: Ending a work session or logging off — close out todos and leave the note clean. For a full uncaptured-knowledge sweep before a reset/compact/handoff, use `stow` instead.
keywords: [wrap, finish, end, log off, done for the day, sign off, close out, wind down, stop]
---
You are wrapping up a work session on the current branch.

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
   disappears when the branch does. See `/bn:repo-reference-update`.
7. If a PR was opened for this branch, link it with `bn_pr <url>` (or `bn pr <url>`) — the note is
   where the next session and the review queue both look for it, and a summary in chat is not a link.
8. Capture the links this session leaves behind: `bn links capture` stores the branch's PR and
   work item, and `bn links add <url> --name "<what it is>" --tags <tags>` takes anything else
   you will want again — the PBI, an Azure portal blade, a dashboard, the doc that answered the
   question. They are person-scoped and cross-machine, so they outlive this worktree; a URL only
   in the note leaves when the worktree does.
9. If the branch's work is merged and finished, run `bn note close` in the shell — promptly, since
   the merge→close gap is what `bn stats metrics` measures as close-out lag. It warns about any PR or
   work item you have not captured.
10. Finish with a 2–3 line summary of what changed and what's left.

Be faithful: only mark todos done that are actually complete.
