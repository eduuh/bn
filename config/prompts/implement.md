---
name: implement
description: "Run the full implementation lifecycle from requirements to deployment. Use when asked to implement or build a feature and no pipeline stage skill applies."
when: "Implementing a feature, building something new, or when 'implement' is mentioned with a feature description."
keywords: [implement, orchestrate, complete, implementation, lifecycle, requirements, deployment, implementing, feature, building, something, new]
arguments: [{ name: request, description: What to implement., required: false }]
---
The main agent owns the implementation lifecycle for **{{request}}**. Establish requirements
and acceptance criteria, use the repository's existing planning gates, implement the scoped
change, verify it, and hand it back.

1. **Orient and scope.** Read the branch context and relevant repository instructions.
   Identify existing work and approved artifacts before starting anything new. Reproduce a
   failure or demonstrate the missing behavior before changing it. Set observable acceptance
   criteria; resolve genuine ambiguity, missing permission, or an irreversible choice with
   the user rather than guessing.

2. **Plan only what is needed.** Follow the task's real planning and approval gates. Reuse
   an approved plan when one exists; otherwise use `plan` when the change needs a plan.
   Do not add specialist pipelines, PRDs, JSON contracts, or extra approval stages unless
   the user or repository explicitly requires them. Record decisions and remaining work
   as they occur.

3. **Delegate selectively.** Use `subagent-orchestration` when substantial research,
   independent review, bulky command output, or non-overlapping implementation work
   benefits from separate contexts. Delegate only those bounded steps through Copilot
   fleet / Claude subagents; do not delegate every phase automatically. Start with the
   smallest useful team and expand only for independent work, without a fixed agent-count
   cap. Use agent types actually available in the harness, not fictional specialist
   prerequisites. If already assigned as a worker, complete your slice without further
   delegation unless the parent explicitly authorizes an independent split.

4. **Implement against the contract.** Make surgical changes matching surrounding style.
   Every changed line must serve the task or an acceptance criterion. Give any worker the
   findings, criteria, repository and revision, allowed write scope, stopping condition,
   and applicable instructions. Worker briefs do not override higher-priority harness
   instructions. Keep integration and shared writes with the main owner. If the scope
   proves wrong, stop and resolve it rather than silently expanding the task.

5. **Verify the integrated result.** Add or update the narrowest tests proving the behavior.
   Run the relevant repository checks on the final revision, confirm the reproduction
   passes, and check every acceptance criterion. Use a fresh reviewer for substantial or
   risky changes when independence adds value; do not blindly duplicate completed checks.
   Fix actionable findings and rerun affected checks. Reverify after a changed base;
   failure or missing evidence is not completion.

6. **Hand back verified work.** Follow the repository's commit, PR, and editor handoff
   requirements and any human deployment gate. Commit only task-owned files. Record
   links and durable findings promptly, close completed work honestly, and capture any
   remaining blocker. Stop only owned workers no longer needed after their results are
   saved. Report what changed, exact validation, remaining uncertainty, and the next action.
