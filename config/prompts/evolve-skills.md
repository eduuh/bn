---
name: evolve-skills
description: "Fold skill reflections back into the skill files as edits you approve. Use after a session where a skill was wrong, missing a step or never picked up."
when: Periodically, or when a workflow skill has been misfiring. Turns accumulated "skill:<name> — …" reflections into concrete edits to the skill files.
keywords: [evolve, improve, refine, meta, update skills, feedback, reflections, tune, self-improve, sharpen]
arguments:
  - { name: skill, description: Limit to one skill by name (e.g. debug). Omit to sweep every skill., required: false }
  - { name: window, description: "How far back to read reflections, e.g. 14d or 4w. Default: 4w.", required: false }
---
You are folding the friction people recorded while *using* the workflow skills back into the
skills themselves. The signal is reflections tagged `skill:<name> — …` (the hook the dispatch
envelopes append to every dispatched skill run). Your output is a **proposed diff the user
approves**, never a silent edit — these files are committed and shared.

For an explicit user-requested policy audit, use the requested policy and current sources as
the evidence; reflections are optional context, not a prerequisite. The "no reflections → stop"
rule applies only to reflection-driven evolution. In both modes, propose concrete diffs and
apply only approved changes.

1. **Gather the evidence.** For an explicit policy audit, start from the requested policy and
   source scope; gather reflections only if useful. For reflection-driven evolution, run
   `bn stats learnings --since <days>` via the harness — it aggregates
   every recorded reflection in one call. Use the window ({{window}}) as days (default the last
   4 weeks, `--since 28`). If a skill was named ({{skill}}), add `--skill <that name>` to filter
   to it; otherwise sweep every `skill:`-tagged entry. If a reflection-driven run has none,
   say so and stop.

2. **Group by skill and find the pattern.** Bucket the reflections per skill name. In a
   reflection-driven run, a one-off gripe is noise; the same complaint twice is signal.
   In an explicit policy audit, compare each source with the requested policy instead.
   For each skill, distill the friction into one sentence: *what the skill tells you to do
   vs. what actually works*.

3. **Read the current skill and locate the fix.** For each skill with real signal, load its
   file: `bn skills list --json` gives each skill's `path` and `scope` (built-ins in the bn repo's
   `config/prompts/`, personal ones in the config dir's `skills/`, repo ones in `.bn/skills/`);
   `bn skills render <name>` shows the live body. Find the exact step or line the evidence
   concerns. Prefer the
   smallest change that resolves it — reword a step, reorder two, add a missing move, cut a step
   that never fires. Don't rewrite a skill that's merely been used a few times.
   For a broad audit, use Copilot fleet / Claude subagents for substantial, non-overlapping
   source groups. Require complete coverage and exact proposed wording from each worker.
   The main owner reconciles cross-skill conflicts and presents one approval package.
   Keep a short single-skill correction local. An assigned audit worker completes its
   source group without launching another layer.

4. **Propose the edit.** For each change show: the skill, the reflections or requested policy
   driving it, and the
   concrete before → after (a diff of the frontmatter `keywords`/`when` or a body step). Preserve
   the house style — numbered steps; `bn_*` names only for real MCP tools, `bn <sub>` for CLI
   commands run via the harness. Keep each skill's identity intact; don't let two skills blur
   into one.

5. **Apply what's approved.** Edit only the skill files the user greenlights. Leave the reflections
   as-is (they're the historical record). Suggest committing the skill changes with a one-line
   conventional subject so the fleet picks them up.

6. **Report.** One line per skill: *skill — N reflections → change / no change*. If a reflection
   pointed at a real gap that's *not* a wording fix (a missing skill, a tool that should exist),
   surface it as a `bn_add todo` rather than forcing it into an edit.

Be conservative: evolve on repeated evidence or an explicit requested policy, keep changes
minimal, and never edit a skill file without showing the diff first. The goal is skills that
get sharper with use, not that drift.
