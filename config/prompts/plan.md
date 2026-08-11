---
name: plan
description: Turn a goal into a verifiable plan artifact — done-when criteria + ordered steps in a Markdown plan file, not prose in chat
when: You have a goal but no plan yet — before you write any code. Turns "here's what I want" into an executable plan file the execution loop can burn down.
keywords: [plan, break down, decompose, scope, design, approach, acceptance criteria, done when, todos, kick off work]
arguments:
  - { name: topic, description: What to plan. Omit to plan the note's current Goal., required: false }
---
You are turning a goal into a **verifiable plan artifact** — not prose in chat. The plan lives in
a Markdown file the execution loop (`plan-work`/`execute`) can burn down: `plans/active.md`,
unless this seed names a bn-owned draft file — then write that file instead. The branch note
stays the index/status layer. You do **not** write code in this skill; you make the next
session's coding unambiguous.

1. **Load the investigated task.** Call `bn_cat` to load the note. If a topic was given
   ({{topic}}), that's the goal — set it with `bn_goal` only when the note has no goal. Otherwise
   use the existing Goal. Read every Research finding first: they are the investigate stage's
   evidence and must shape the plan. Validate only missing details; do not repeat settled research
   or invent facts when evidence is absent.

2. **Write the plan artifact.** Run `bn plan init` (skip it when the seed already names a draft
   file) and write the plan Markdown:
   - the goal, restated in one sharp line;
   - 2–5 **done-when criteria** — observable, checkable outcomes ("login succeeds on 4G within
     10s", "this repo's test gate is green", "no new lint warnings"). Name the repo's *own*
     commands, whatever its ecosystem is; a criterion quoting a tool this repo does not use is not
     checkable. These are the contract the execution loop verifies against; without them "done" is
     a guess.
   - **ordered steps** — one action per step, in the order you'd do them; split any step with a
     real fork or risk. The list should read as the path from here to the done-when criteria.
   - risks and open questions.
   Do not decompose the plan through repeated `bn_add todo`/`bn_add done-when` calls — the
   artifact is the contract.

3. **Log the decisions.** For any choice with a tradeoff — a library, a data shape, an approach
   you picked over an alternative — record it now with `bn_add decision`, including *why X over
   Y*. Decide once, here, so the execution session doesn't re-litigate it.

4. **Name the unknowns.** Anything the persisted findings still do not settle is `bn_add research`.
   If an unknown is big enough to block the plan, do not guess or silently widen the plan: return
   the branch to `investigate` with `bn pipeline set investigate` and state the focused question.

5. **Report.** Restate the goal in one line, then: the done-when criteria, the ordered steps, and
   the single first step you'd start. Say explicitly whether the plan is ready to execute or
   gated on an `investigate`. Keep it scannable — the plan file holds the detail.
