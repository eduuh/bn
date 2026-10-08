---
name: repo-reference
description: "Read and write back durable facts about this repo (how it builds, how it fails, conventions). Use before working out a repo's build or quirks, and after learning something that stays true."
when: You are about to work out how this repo builds, runs, deploys, or fails — or you hit an error that smells like it has bitten someone before. Check the reference notes first.
keywords: [record, capture, write down, learned, fold back, update notes, reference, reference notes, repo knowledge, how does this repo build, how do I run this, where does this live, why does this fail, gotcha, prior knowledge, onboarding, architecture, what do we know]
arguments:
  - { name: question, description: What you need to know about the repo. Omit to just read the index., required: false }
---
Before you re-derive knowledge about this repo from its source, find out whether a past session
already wrote it down. That is what the **reference notes** are: durable facts about *this
repository* — how it builds, where the seams are, which failures mislead — living beside the branch
notes and outliving all of them.

1. **Read the index first.** Call `bn_ref` with no name (CLI: `bn ref --repo`). You get every
   note's name and one-line summary and nothing else — a handful of tokens. Do this before
   assuming the notes are empty; "there's probably nothing" is how knowledge gets re-derived for
   the fourth time.

2. **Open at most what you need.** Call `bn_ref` with the one name that bears on {{question}}
   (CLI: `bn ref <name> --repo`). Two notes is usually one too many — the index summaries exist so
   you can choose. Global coding standards come through the same tool: `scope: "global"`.

3. **Trust it the way it asks to be trusted.** Every bullet should carry where it came from and
   when it was checked — `(src: path/to/file, 2026-09-03)` or `(observed, 2026-09-03)`. A sourced
   fact is a shortcut: go straight to that file rather than searching for it. An **unsourced**
   bullet means nobody verified it — treat it as a lead, not a fact. A fact whose date is old
   enough to be suspect is also a lead: check it, then correct the note (see
   the recording section below).

4. **When the notes are wrong, say so and fix them.** Finding that a recorded fact no longer holds
   is a result worth keeping. Correct the bullet in place — `bn ref edit <name> --repo` — rather
   than leaving a known-false claim for the next session to trip on.

5. **When the notes are silent, say that too.** Do not invent a fact and attribute it to the
   notes. Work it out from the code, then record what you learned so the next session doesn't
   repeat the work.

Report what you used: name the note and the fact you acted on, so the answer can be checked
against its source rather than taken on faith.

## Recording what you learned (was repo-reference-update)

Record {{question}} where the next session will find it. The reference notes are what we know about
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
