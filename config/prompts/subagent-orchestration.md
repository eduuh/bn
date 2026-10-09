---
name: subagent-orchestration
description: "Choose and sequence subagents through an implementation workflow, deciding when to run autonomously. Use when delegating to several subagents or managing workflow phases."
when: Orchestrating multiple agents, managing workflow phases, or determining autonomous execution mode.
keywords: [subagent-orchestration, subagents, orchestration, guide, guides, subagent, coordination, implementation, workflows, orchestrating, multiple, agents]
---
# Bounded subagent orchestration

1. **Decide whether delegation helps.** Use Copilot fleet / Claude subagents for substantial
   research, independent review, or bulky build/test output. Keep short known-file lookups,
   small edits, and one continuous trace local.

2. **Choose the smallest useful team.** Assign only work that can proceed independently or
   benefits from fresh context. Expand when another concrete assignment warrants it, not to
   fill roles. There is no fixed agent-count cap.

3. **Use available capabilities.** Discover the harness's actual agent types. Do not require
   named specialists bn does not ship. Use an available suitable worker with an explicit
   brief; preserve any specialist the user specifically requested and report if it is
   unavailable. Use ordinary harness delegation; Copilot fleet does not imply a registered
   dynamic workflow. Start such a workflow only when explicitly requested or instructed.

4. **Brief each worker.** Give the question or outcome, repository and revision, scope,
   evidence required, allowed writes, dependencies, stopping condition, and result format.
   Include applicable user constraints and repository instructions. Workers return concise
   findings with sources, commands and exit codes, uncertainties, and artifact locations,
   rather than transcripts.

5. **Keep one owner.** The main agent owns requirements, integration, shared state,
   approvals, and delivery. Give implementation workers non-overlapping write scopes or
   isolated worktrees. A worker does not delegate further unless the owner explicitly
   authorizes an independent split. If you are already an assigned worker, complete that
   slice yourself.

6. **Reconcile evidence.** Follow up with the same worker on the same task when its retained
   context helps. Start fresh for unrelated work; do not maintain unrelated worker pools.
   Resolve contradictory findings using the user's constraints and repository evidence.
   Do not silently choose a convenient answer, reread the entire search trail, or repeat
   checks without a concrete reason. Escalate missing permission, genuine ambiguity, or a
   changed scope.

7. **Verify and finish.** Run the relevant checks on the integrated revision and confirm
   the acceptance criteria. Use a fresh review when independence adds value, not as an
   automatic duplicate of every completed check. Capture worker results and stop only
   owned workers that are no longer needed. Follow the repository's commit, PR, and editor
   handoff requirements. Report evidence, remaining uncertainty, and the next action.

Follow the task's existing planning and approval gates, including pauses for human decisions.
Do not manufacture PRDs, specialist pipelines, document requirements based on file counts,
or extra approval stages merely because multiple agents are available.
