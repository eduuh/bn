---
name: lavish-plan
description: Draft a plan (or diagram/report) visually with the Lavish AXI, then distill the agreed result into the plan artifact — done-when criteria + ordered steps, not just an HTML artifact.
when: You want to think a plan, comparison, or design through visually (rich HTML / Mermaid) with the human in the loop, and land the agreed outcome in the plan artifact so it can be executed and measured.
keywords: [lavish, lavish-plan, lavish-axi, axi, plan, visualize, visualization, html artifact, mermaid, diagram, design, report, save plan, branch note]
arguments:
  - { name: topic, description: What to plan or visualize. Omit to use the note's current Goal., required: false }
---
You are using **Lavish** — an [AXI](https://axi.md) that opens agent-generated **HTML artifacts**
in a local browser so a human can annotate elements, edit Mermaid diagrams as whiteboards, and send
feedback back to you (https://github.com/kunchenguid/lavish-axi). Here it is the *thinking surface*
for a plan; the **plan artifact is the durable record**. An HTML artifact nobody can execute is not
a plan — this skill ends with the plan written into `plans/active.md`, exactly like `plan`.

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
   HTML, is what `plan-work` executes. Record every tradeoff you settled in Lavish with
   `bn_add decision` (*why X over Y*), and anything still unsettled with `bn_add research`; if an
   unknown blocks the plan, return to `investigate`.

4. **Link the artifact, keep the plan self-sufficient.** Export the Lavish artifact
   (`lavish-axi export --out <path>`) and record its location with a `bn_add decision` so it's
   findable — but write the plan so the next session can execute it **without** opening the HTML.

5. **Report.** Restate the Goal in one line, then the done-when criteria, the ordered steps, the
   single first step to start, and where the Lavish artifact lives. Say explicitly whether the
   plan is ready to execute or gated on an `investigate`. Keep it scannable — the plan file holds
   the detail.
