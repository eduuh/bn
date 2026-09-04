---
name: repo-reference-update
description: Fold what this session learned about the repo back into its reference notes — durable facts only, each with its source
when: You just learned something about this repo that will still be true next month — a root cause, a build gotcha, where a subsystem actually lives. Record it before the session ends and the knowledge dies with it.
keywords: [reference notes, record, capture, write down, learned, gotcha, root cause, durable knowledge, update notes, document, fold back]
arguments:
  - { name: fact, description: The thing you learned. Omit to sweep the session for anything uncaptured., required: false }
---
Record {{fact}} where the next session will find it. The reference notes are what we know about
*this repository*, so they only accept knowledge shaped like that.

**The one test: will this still be true next month, for anyone working on this repo?**

- Yes → a reference note. "The post-checkout hook prints `node: not found` in a non-login shell,
  so bn reports a failed switch that actually succeeded."
- No → the branch note (`bn_add`), or `bn learned` for a personal lesson. "The test on my branch
  fails because I forgot to rebuild the dependency" is not repo knowledge, it is Tuesday.

**How to write one:**

1. **Pick the note the fact belongs to.** Read the index first (`bn_ref` with no name) — a fact
   filed under a new name nobody looks at is a fact lost. Create a new note only when no existing
   one is a home for it.

2. **One fact per bullet, with its provenance.** Append with `bn_ref_append`
   (CLI: `bn ref append <name> --repo --section gotchas "<fact>"`), section `facts` or `gotchas`.
   Every bullet ends with where it came from and when you checked it:
   `(src: web/.nvmrc, 2026-09-03)` for something read in the source, `(observed, 2026-09-03)`
   for something you saw happen. **Do not skip this.** An unsourced bullet is indistinguishable
   from a guess, and a note full of guesses is worse than no note — it gets believed.

3. **Say what you actually established, not what you suspect.** If you fixed a symptom without
   proving the cause, record the symptom and the workaround, and say the cause is unconfirmed.
   Overclaiming is how a note becomes a trap.

4. **Correct, don't accumulate.** If the note already says something that this session proved
   wrong, **edit that bullet** (`bn ref edit <name> --repo`). Appending a contradiction leaves the
   next reader to guess which line is current. `bn_ref_append` returns `appended: false` when the
   exact fact is already recorded — that is a clean no-op, not a failure.

5. **Never record:** credentials, tokens, cookies, session or conversation IDs, tenant or account
   identifiers, machine names, tunnel URLs, or anything tied to one ticket. The notes tree is a
   synced git repo; treat every write as publication.

Then say what you recorded and where, in one line, so it can be checked.
