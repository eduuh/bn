---
name: repo-reference
description: Read what past sessions already learned about this repo before re-deriving it from the code
when: You are about to work out how this repo builds, runs, deploys, or fails — or you hit an error that smells like it has bitten someone before. Check the reference notes first.
keywords: [reference, reference notes, repo knowledge, how does this repo build, how do I run this, where does this live, why does this fail, gotcha, prior knowledge, onboarding, architecture, what do we know]
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
   `/bn:repo-reference-update`).

4. **When the notes are wrong, say so and fix them.** Finding that a recorded fact no longer holds
   is a result worth keeping. Correct the bullet in place — `bn ref edit <name> --repo` — rather
   than leaving a known-false claim for the next session to trip on.

5. **When the notes are silent, say that too.** Do not invent a fact and attribute it to the
   notes. Work it out from the code, then record what you learned so the next session doesn't
   repeat the work.

Report what you used: name the note and the fact you acted on, so the answer can be checked
against its source rather than taken on faith.
