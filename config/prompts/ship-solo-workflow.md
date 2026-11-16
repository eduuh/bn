---
name: ship-solo-workflow
internal: true
description: "Deliver one bounded task end to end. Use when implementing without an approved plan or separate pipeline stage."
when: A task needs one implementation owner, investigation, verification, review and delivery.
keywords: [ship, implement, build, single session, deliver]
arguments:
  - { name: task, description: Task to deliver., required: false }
  - { name: url, description: Source issue or PR., required: false }
---
Deliver the task ({{task}}), grounded on its source ({{url}}), with one implementation owner.
Investigation, implementation, and review are phases, not mandatory worker slots. Delegate only
substantial independent work. Workers do not delegate further without the owner's approval.

1. **Frame.** Run `workspan discover` with the task, inspect the worktree and assigned Workspan
   task/context, and read applicable instructions. Resolve genuine ambiguity with `ask_user`.
   Preserve existing work; never auto-stash or rewrite a shared/published branch.
2. **Demonstrate the problem.** Reproduce the failure or demonstrate the missing behavior before
   editing. Establish where the change lives, acceptance criteria, and what could break.
   Follow evidence past the first plausible answer. Save only actionable findings on the assigned
   task; Entire captures the investigation history.
3. **Decide.** Settle the smallest implementation and observable acceptance criteria. Ask on real
   product forks or missing permission. Do not manufacture a planning stage or plan file.
4. **Implement.** Match nearby conventions; touch only what the task and its safety require.
   Add focused regression cases for changed behavior. Keep the task's acceptance checklist
   current rather than maintaining a parallel session ledger.
5. **Verify and review.** Prove the original reproduction now passes and run `workspan verify`
   plus required repository checks. Review the diff against the criteria; use fresh independent
   review for substantial or risky changes. Fix concrete findings and rerun affected checks.
   Green tests alone are not proof of an outcome nobody demonstrated.
6. **Deliver.** Follow commit privacy and the repository's base-refresh/editor handoff rules.
   Stage only owned files, commit conventionally, and run any gates invalidated by a rebase.
   Push the exact gated commit and open the authorized PR. Record its link and remaining actions
   on the existing task/provider; mark the task done only when its criteria are met.
7. **Report.** Lead with the outcome and PR. State incomplete or blocked work plainly.

Entire owns captured sessions and checkpoints. Do not create a competing transcript, memory
ledger, or handoff packet. Preserve old historical files; their existence is not an execution
prerequisite.
