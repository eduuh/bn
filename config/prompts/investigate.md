---
name: investigate
description: "Research a scoped question and record evidence. Use for substantial requested research or a dispatched investigation stage, never routine code inspection."
when: Substantial research was explicitly requested or selected by a pipeline.
keywords: [investigate, research workflow, feasibility study, research spike]
arguments:
  - { name: question, description: Question to answer; otherwise use the assigned task., required: false }
---
This workflow is read-only research, not a prerequisite for ordinary questions or implementation.
Do not add planning gates, implementation, or PR creation to a research request.

1. **Frame.** Use the question ({{question}}), otherwise the supplied task packet and context.
   State the scoped questions. Read existing evidence before looking for missing facts.
2. **Investigate.** Trace code, docs, and read-only command evidence. Touch no source. Delegate
   only substantial independent evidence gathering; keep one continuous trace local. Assigned
   workers complete their slice without launching another layer.
3. **Record findings.** Put concise answers, `path:symbol` or command evidence, reproduction,
   and uncertainties in the assigned task's existing context/provider or the artifact explicitly
   named by the seed. Do not create a second session log; Entire captures the reasoning history.
4. **Resolve open threads.** In a dispatched pipeline, use
   `workspan pipeline investigate finish --unknown "<question>"` for unresolved independent
   questions, or `--blocked "<input needed>"` when a person must decide. Do not duplicate an
   active worker or widen the authorized question.
5. **Preserve repo knowledge.** Verified contributor-wide facts belong in repo references with
   their source and date (`repo-reference`). Correct contradicted facts rather than appending
   another claim.
6. **Finish.** Standalone research returns findings and stops. A dispatched investigation runs
   `workspan pipeline investigate finish` only after its evidence is recorded, then hands off
   without planning or implementing in that context.
