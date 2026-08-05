---
name: plan
description: Turn a goal into a verifiable plan written into the branch note — done-when criteria + ordered todos, not prose in chat
when: You have a goal but no plan yet — before you write any code. Turns "here's what I want" into an executable checklist in the note.
keywords: [plan, break down, decompose, scope, design, approach, acceptance criteria, done when, todos, kick off work]
arguments:
  - { name: topic, description: What to plan. Omit to plan the note's current Goal., required: false }
---
You are turning a goal into a **verifiable plan written into the branch note** — not prose in
chat. The plan lives in the note so `plan-work` can execute it and `bn_metrics` can measure it.
You do **not** write code in this skill; you make the next session's coding unambiguous.

1. **Load the investigated task.** Call `bn_cat` to load the note. If a topic was given
   ({{topic}}), that's the goal — set it with `bn_goal` only when the note has no goal. Otherwise
   use the existing Goal. Read every Research finding first: they are the investigate stage's
   evidence and must shape the plan. Validate only missing details; do not repeat settled research
   or invent facts when evidence is absent.

2. **Write done-when criteria.** Decide how you'll *know* it's done, and record each as
   `bn_add` in the `done-when` section — observable, checkable outcomes ("login succeeds on 4G
   within 10s", "`cargo test` green", "no new clippy warnings"). These are the contract
   `plan-work` verifies against; without them "done" is a guess. 2–5 is usually right.

3. **Decompose into ordered todos.** Break the work into `bn_add todo` items — **one checkbox =
   one action**, in the order you'd do them. Not "build the thing"; not a five-verb run-on. If a
   step has real risk or a fork, split it. The list should read as the path from here to the
   done-when criteria.

4. **Log the decisions.** For any choice with a tradeoff — a library, a data shape, an approach
   you picked over an alternative — record it now with `bn_add decision`, including *why X over
   Y*. Decide once, here, so the execution session doesn't re-litigate it.

5. **Name the unknowns.** Anything the persisted findings still do not settle is `bn_add research`.
   If an unknown is big enough to block the plan, do not guess or silently widen the plan: return
   the branch to `investigate` with `bn pipeline set investigate` and state the focused question.

6. **Report.** Restate the goal in one line, then: the done-when criteria, the ordered todos, and
   the single first todo you'd start. Say explicitly whether the plan is ready to execute or
   gated on an `investigate`. Keep it scannable — the note holds the detail.

If planning this surfaced a rough edge in *this* skill — a step you skipped, an instruction that
misfired, a move that was missing — capture it with `bn reflect "skill:plan — <what to change>"`
(via the harness). That's how this skill evolves; `evolve-skills` folds those back into this file.
