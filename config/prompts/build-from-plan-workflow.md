---
name: build-from-plan-workflow
internal: true
description: "Execute the active plan file to its done-when criteria. Use when a plan already exists and is being implemented."
when: An active plan already exists and is ready to execute.
keywords: [work, execute, build, implement, burn down, run the plan, verify]
arguments:
  - { name: continuous, description: "If truthy (true/yes/1), loop through every step; otherwise do the next one.", required: false }
---
The approved plan is the contract: execute it, do not silently replace it.

1. **Load.** Run `workspan plan status` and read the active file at `active_abs`;
   `workspan plan path` also prints its location. Read the assigned task's context for blockers.
   If there is no plan, stop and recommend `plan`, unless this seed explicitly owns creating one.
2. **Implement the next step.** Take the first unchecked step. Match nearby code and touch only
   what it or an acceptance criterion requires. For mechanical changes, find all affected sites
   first and batch the coherent edits.
3. **Update actual progress.** Check a step off only when it is complete. Put newly discovered
   actionable blockers and scope decisions in the existing task context. Ask on a genuine fork;
   do not expand the approved plan silently.
4. **Handle failures.** A reproducible product failure belongs to `debug`. Missing build
   prerequisites do not automatically justify a debugging workflow. Stop for the existing gate
   when new evidence changes the approved scope.
5. **Loop or stop.** If continuous ({{continuous}}) is truthy, repeat until the steps are complete;
   otherwise finish the next step and stop. Do not close the parent task early.
6. **Verify.** Run `workspan verify` and required repo checks, and prove each done-when criterion.
   Re-run affected gates after changes or a rebase. For a new guard, demonstrate that a probe
   violation fails before removing the probe.
7. **Deliver.** Follow commit privacy and repository delivery rules. Push the exact gated commit
   if publishing is authorized, record the PR on the existing task/provider, and close the task
   only when its criteria are met. Preserve verified contributor-wide facts in repo references.
   Entire captures session history; do not duplicate it in a session ledger.
