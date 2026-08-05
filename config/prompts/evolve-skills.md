---
name: evolve-skills
description: Fold skill:* reflections back into the skill markdown — propose concrete edits you approve
when: Periodically, or when a workflow skill has been misfiring. Turns accumulated "skill:<name> — …" reflections into concrete edits to the skill files.
keywords: [evolve, improve, refine, meta, update skills, feedback, reflections, tune, self-improve, sharpen]
arguments:
  - { name: skill, description: Limit to one skill by name (e.g. debug). Omit to sweep every skill., required: false }
  - { name: window, description: "How far back to read reflections, e.g. 14d or 4w. Default: 4w.", required: false }
---
You are folding the friction people recorded while *using* the workflow skills back into the
skills themselves. The signal is reflections tagged `skill:<name> — …` (the closing hook every
workflow skill writes). Your output is a **proposed diff the user approves**, never a silent edit
— these files are committed and shared.

1. **Gather the reflections.** Read reflections over the given window ({{window}}) — default the
   last 4 weeks — via the harness: `bn weekly` / `bn stand` surface them, or read the daily day
   files under the branch-notes root. Keep only lines matching the `skill:` convention. If a skill
   was named ({{skill}}), filter to `skill:{{skill}} — …` for that one. If there are none, say so
   and stop — nothing to evolve.

2. **Group by skill and find the pattern.** Bucket the reflections per skill name. A one-off gripe
   is noise; the same complaint twice is signal. For each skill, distill the recurring friction
   into one sentence: *what the skill tells you to do vs. what actually works*.

3. **Read the current skill and locate the fix.** For each skill with real signal, load its file
   at `skills/<name>.md` (list them with `bn skills`; render the live body with
   `bn skills render <name>`). Find the exact step or line the reflection is about. Prefer the
   smallest change that resolves it — reword a step, reorder two, add a missing move, cut a step
   that never fires. Don't rewrite a skill that's merely been used a few times.

4. **Propose the edit.** For each change show: the skill, the reflections driving it, and the
   concrete before → after (a diff of the frontmatter `keywords`/`when` or a body step). Preserve
   the house style — numbered steps, `bn_*` tool references, the closing reflect hook. Keep each
   skill's identity intact; don't let two skills blur into one.

5. **Apply what's approved.** Edit only the skill files the user greenlights. Leave the reflections
   as-is (they're the historical record). Suggest committing the skill changes so the fleet picks
   them up — a one-line conventional subject via `notes-commit` fits.

6. **Report.** One line per skill: *skill — N reflections → change / no change*. If a reflection
   pointed at a real gap that's *not* a wording fix (a missing skill, a tool that should exist),
   surface it as a `bn_add todo` rather than forcing it into an edit.

Be conservative: evolve on repeated evidence, keep changes minimal, and never edit a skill file
without showing the diff first. The goal is skills that get sharper with use, not that drift.
