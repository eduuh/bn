---
name: investigate
description: "Research a scoped question and record evidence for a research or pipeline task. Use when explicitly asked for a substantial investigation or selected by a research workflow, never for quick questions or routine code inspection."
when: The user explicitly requests a substantial research investigation, invokes this skill, or a pipeline workflow selects it. Do not activate for quick lookups, ordinary explanations, locating a symbol, or reading code while implementing or debugging.
keywords: [investigate, research workflow, feasibility study, research spike, pipeline investigation]
arguments:
  - { name: question, description: The question to answer. Omit to pick the note's top open research item., required: false }
---
This is an opt-in research workflow, not a prerequisite for answering questions or inspecting
code. A quick lookup stays a direct answer, even if the request says "investigate" or "look into".
Do not introduce branch-note ceremony, delegation, planning gates, implementation, or PR creation
into an ordinary question. For standalone research, return the findings and stop; pipeline handoff
below applies only when that workflow explicitly dispatched the task.

You are the pipeline's **Investigate** stage when this workflow runs inside the pipeline: answer
the task's scoping questions in **read-only** mode, persist evidence in the branch note, and hand a
bounded problem to a separate planner. Standalone research stays read-only and ends with findings;
it does not implement a fix or open a PR.

1. **Frame the question.** Call `bn_cat` for context. Use the question ({{question}}) if given; otherwise take
   the top open `research` item from the note. State it in one sharp line — a vague question yields
   a vague finding. If it's actually several questions, name them and take them in order.

2. **Investigate — read, don't write.** Trace the code, read the docs, run *read-only* commands
   (grep, tests in report mode, `--help`, log inspection). Touch no source.
   For a substantial investigation, assign bounded evidence gathering to Copilot fleet / Claude
   subagents and retain only concise findings with paths, sources, and uncertainties in the main
   context. Split genuinely independent questions, not one continuous trace across workers.
   A short known-file lookup stays local. If already an assigned investigation worker, complete
   your slice rather than launching another layer.
   Follow the evidence
   past the first plausible answer — the goal is to be *right*, not fast. When you learn something
   non-obvious about the codebase or tooling on the way, `bn_ref_append` it into the repo's
   reference notes with its source and today's date.

3. **Persist findings.** Record each concrete answer with `bn_add research`, including
   `path:symbol` or command evidence rather than assertions. For a long supporting write-up, also
   save `$(bn --path)/<topic>.md`, but the note's Research items are the durable handoff the
   planner receives.

4. **Resolve open threads.** Use fresh sub-investigators only for unresolved questions with
   independent evidence paths. Do not fan out questions answered by one cheap lookup, duplicate
   an active worker, or dispatch the same unknown again without new evidence or a narrower
   question. If focused unknowns remain, run `bn pipeline investigate finish
   --unknown "<question>"` once per unknown; the driver fans them out to fresh sub-investigators
   and merges their evidence. If the task cannot be scoped without a person, run `bn pipeline
   investigate finish --blocked "<specific input needed>"` so a typed Planning attention item is
   raised instead of guessing.

5. **Promote the durable findings.** An investigation is where repo knowledge is *made*. Before
   handing off, re-read your findings and ask which are true about the repo regardless of this
   task. Those go to `bn_ref_append` with their source and today's date (see
   `/bn:repo-reference`) — the branch note is scoped to this task, the reference notes
   are what the next investigation starts from. Correct a note that this work proved wrong
   rather than appending a contradiction.

6. **Hand off.** In the pipeline stage, when the task is scoped and
   findings are recorded, run `bn pipeline investigate finish`. This advances the branch to `plan`
   and launches a fresh planner seeded with these findings. Report the finding count and stop; do
   not plan or implement in that context.
