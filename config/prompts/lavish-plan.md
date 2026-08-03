---
name: lavish-plan
description: Draft a plan (or diagram/report) visually with the Lavish AXI, then distill the agreed result into the branch note — done-when criteria + ordered todos, not just an HTML artifact.
when: You want to think a plan, comparison, or design through visually (rich HTML / Mermaid) with the human in the loop, and land the agreed outcome in the branch note so it can be executed and measured.
keywords: [lavish, lavish-plan, lavish-axi, axi, plan, visualize, visualization, html artifact, mermaid, diagram, design, report, save plan, branch note]
arguments:
  - { name: topic, description: What to plan or visualize. Omit to use the note's current Goal., required: false }
---
You are using **Lavish** — an [AXI](https://axi.md) that opens agent-generated **HTML artifacts**
in a local browser so a human can annotate elements, edit Mermaid diagrams as whiteboards, and send
feedback back to you (https://github.com/kunchenguid/lavish-axi). Here it is the *thinking surface*
for a plan; the **branch note is the durable record**. An HTML artifact nobody can execute is not a
plan — this skill ends with the plan written into the note, exactly like `plan`.

This skill is *planning through* Lavish, not Lavish itself: the upstream agent-skill installed by
`npx skills add kunchenguid/lavish-axi --skill lavish` owns the bare name `lavish` and provides the
AXI, while this one drives it toward a durable branch note. Two names, deliberately — bn materializes
its skills into `~/.claude/skills` too, so sharing the name would collide with the upstream file.

1. **Preflight the AXI.** The skill runs Lavish through `npx -y lavish-axi`, so try
   `npx -y lavish-axi update --check` (or `command -v lavish-axi`). If it is unavailable and you
   are offline, don't block: fall back to the text-only `plan` skill and note that the visual pass
   was skipped. The full upstream skill + optional SessionStart hooks come from
   `npx skills add kunchenguid/lavish-axi --skill lavish` or bn's `./install.sh --agents`
   (hooks stay behind `BN_SETUP_LAVISH_HOOKS=1`) — point at those, don't `npm install -g` ad hoc.

2. **Orient on the branch.** Call `bn_context` (and `bn_cat` for detail) for the Goal and open
   work, so the plan lands on the right branch note. If a topic was given ({{topic}}), that's the
   subject; otherwise plan the note's current Goal. Set the Goal with `bn_goal` only if none exists.

3. **Draft it visually with Lavish.** Produce a rich HTML artifact for the plan — a product or
   technical plan, a comparison, a table, or a Mermaid diagram of the approach — and open it with
   `npx -y lavish-axi <file.html>`. Use `lavish-axi design` for agent-facing visualization
   guidance. Iterate with the human through Lavish annotations and messages until the plan is
   **agreed**, not just drawn.

4. **Distill the agreed plan into the note.** This is the point of the skill. Translate the artifact
   into the branch note like `plan` does — the note, not the HTML, is what `plan-work` executes:
   - `bn_add done-when` — 2–5 observable, checkable outcomes ("`cargo test` green", "no new clippy
     warnings"). These are the contract; without them "done" is a guess.
   - `bn_add todo` — ordered, **one checkbox = one action** steps, in the order you'd do them.
   - `bn_add decision` — every tradeoff you settled in Lavish, including *why X over Y*.
   - `bn_add research` — anything still unsettled; if it blocks the plan, return to `investigate`.

5. **Link the artifact, keep the note self-sufficient.** Export the Lavish artifact
   (`lavish-axi export --out <path>`) and record its location with a `bn_add decision` so it's
   findable — but write the note so the next session can execute it **without** opening the HTML.

6. **Report.** Restate the Goal in one line, then the done-when criteria, the ordered todos, the
   single first todo to start, and where the Lavish artifact lives. Say explicitly whether the plan
   is ready to execute or gated on an `investigate`. Keep it scannable — the note holds the detail.

If this surfaced a rough edge in *this* skill — a missing step, an instruction that misfired —
capture it with `bn reflect "skill:lavish-plan — <what to change>"` (via the harness). That is how this
skill evolves; `evolve-skills` folds those notes back into this file.
