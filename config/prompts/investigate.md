---
name: investigate
description: Investigate a task, persist evidence in the branch note, and either hand scoped findings to planning or graduate a bounded scout fix into a PR
when: There's an open question you have to answer before you can plan or build — how does X work, is Y feasible, where does Z live. Investigation comes first; in an ad-hoc scout, a clear bounded fix may follow.
keywords: [investigate, research, scout, explore, look into, understand, feasibility, spike, find out, how does, where is]
arguments:
  - { name: question, description: The question to answer. Omit to pick the note's top open research item., required: false }
---
You are the pipeline's **Investigate** stage when this workflow runs inside the pipeline: answer
the task's scoping questions in **read-only** mode, persist evidence in the branch note, and hand a
bounded problem to a separate planner. When the same workflow is wrapped by an ad-hoc scout
dispatch, investigation is still first and read-only, but once the findings make the change clear
you may continue in the same session into a bounded fix and PR instead of handing off to planning.
Do not start coding before the evidence says what to change.

1. **Frame the question.** Call `bn_cat` for context. Use the question ({{question}}) if given; otherwise take
   the top open `research` item from the note. State it in one sharp line — a vague question yields
   a vague finding. If it's actually several questions, name them and take them in order.

2. **Investigate — read, don't write.** Trace the code, read the docs, run *read-only* commands
   (grep, tests in report mode, `--help`, log inspection). Touch no source. Follow the evidence
   past the first plausible answer — the goal is to be *right*, not fast. When you learn something
   non-obvious about the codebase or tooling on the way, `bn learned "…"` it.

3. **Persist findings.** Record each concrete answer with `bn_add research`, including
   `path:symbol` or command evidence rather than assertions. For a long supporting write-up, also
   save `$(bn --path)/<topic>.md`, but the note's Research items are the durable handoff the
   planner receives.

4. **Resolve open threads.** If focused unknowns remain, run `bn pipeline investigate finish
   --unknown "<question>"` once per unknown; the driver fans them out to fresh sub-investigators
   and merges their evidence. If the task cannot be scoped without a person, run `bn pipeline
   investigate finish --blocked "<specific input needed>"` so a typed Planning attention item is
   raised instead of guessing.

5. **Promote the durable findings.** An investigation is where repo knowledge is *made*. Before
   handing off, re-read your findings and ask which are true about the repo regardless of this
   task. Those go to `bn_ref_append` with their source and today's date (see
   `/bn:repo-reference-update`) — the branch note is scoped to this task, the reference notes
   are what the next investigation starts from. Correct a note that this work proved wrong
   rather than appending a contradiction.

6. **Hand off, or graduate the scout.** In the pipeline stage, when the task is scoped and
   findings are recorded, run `bn pipeline investigate finish`. This advances the branch to `plan`
   and launches a fresh planner seeded with these findings. Report the finding count and stop; do
   not plan or implement in that context.

   In an ad-hoc scout envelope, skip the pipeline handoff. If the findings show a bounded fix you
   can verify, implement it in this same session, record decisions as you go, and open a PR. If
   the right result is a recommendation, a person-blocked handoff, or nothing to fix, stay
   report-only.
