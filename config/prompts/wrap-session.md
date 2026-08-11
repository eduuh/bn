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
7. If a PR was opened for this branch, link it with `bn_pr <url>` (or `bn pr <url>`) — the note is
   where the next session and the review queue both look for it, and a summary in chat is not a link.
8. If the branch's work is merged and finished, run `bn close` in the shell — promptly, since
   the merge→close gap is what `bn metrics` measures as close-out lag.
9. Finish with a 2–3 line summary of what changed and what's left.

Be faithful: only mark todos done that are actually complete.
