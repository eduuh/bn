---
name: plan
description: "Turn a goal into a verifiable plan file: done-when criteria plus ordered steps, with an optional visual Lavish pass. Use before coding, or when asked to make a plan, diagram or report."
when: You have a goal but no plan yet — before you write any code. Turns "here's what I want" into an executable plan file the execution loop can burn down.
keywords: [lavish, visualize, mermaid, diagram, plan, break down, decompose, scope, design, approach, acceptance criteria, done when, todos, kick off work]
arguments:
  - { name: topic, description: What to plan. Omit to plan the note's current Goal., required: false }
---
You are turning a goal into a **verifiable plan artifact** — not prose in chat. The plan lives in
a Markdown file the execution loop (`build-from-plan`/`execute`) can burn down: `plans/active.md` beside
the branch note (not in the repo), unless this seed names a bn-owned draft file — then write that
file instead. The branch note
stays the index/status layer. You do **not** write code in this skill; you make the next
session's coding unambiguous.

1. **Load the investigated task.** Call `bn_cat` to load the note. If a topic was given
   ({{topic}}), that's the goal — set it with `bn_goal` only when the note has no goal. Otherwise
   use the existing Goal. Read every Research finding first: they are the investigate stage's
   evidence and must shape the plan. Validate only missing details; do not repeat settled research
   or invent facts when evidence is absent.

2. **Write the plan artifact.** Run `bn plan init` (skip it when the seed already names a draft
   file); it creates the file and prints its path. Write the plan Markdown there:
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

## Visual mode (Lavish)

Use when the human wants to think the plan through visually (HTML, Mermaid, comparison tables) before it is written down. The plan artifact stays the durable record; an HTML artifact nobody can execute is not a plan.

1. **Orient on the branch.** Call `bn_context` (and `bn_cat` for detail) for the Goal and open
   work, so the plan lands on the right branch note. If a topic was given ({{topic}}), that's the
   subject; otherwise plan the note's current Goal. Set the Goal with `bn_goal` only if none exists.

2. **Draft it visually with Lavish.** Produce a rich HTML artifact for the plan — a product or
   technical plan, a comparison, a table, or a Mermaid diagram of the approach — and open it with
   `npx -y lavish-axi <file.html>`. If that fails (offline, AXI unavailable), don't block: fall
   back to the text-only `plan` skill and note that the visual pass was skipped (the full upstream
   skill comes from `npx skills add kunchenguid/lavish-axi --skill lavish` or bn's
   `./install.sh --agents` — don't `npm install -g` ad hoc). Use `lavish-axi design` for
   agent-facing visualization guidance. Iterate with the human through Lavish annotations and
   messages until the plan is **agreed**, not just drawn.

3. **Distill the agreed plan into the plan artifact.** This is the point of the skill. Land the
   result exactly as `plan` does — run `bn plan init` and write `plans/active.md` (goal, 2–5
   observable done-when criteria, ordered steps, risks, open questions); the artifact, not the
   HTML, is what `build-from-plan` executes. Record every tradeoff you settled in Lavish with
   `bn_add decision` (*why X over Y*), and anything still unsettled with `bn_add research`; if an
   unknown blocks the plan, return to `investigate`.

4. **Link the artifact, keep the plan self-sufficient.** Export the Lavish artifact
   (`lavish-axi export --out <path>`) and record its location with a `bn_add decision` so it's
   findable — but write the plan so the next session can execute it **without** opening the HTML.

5. **Report.** Restate the Goal in one line, then the done-when criteria, the ordered steps, the
   single first step to start, and where the Lavish artifact lives. Say explicitly whether the
   plan is ready to execute or gated on an `investigate`. Keep it scannable — the plan file holds
   the detail.
