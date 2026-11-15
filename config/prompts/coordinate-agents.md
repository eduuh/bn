---
name: coordinate-agents
description: "Run several work agents from one lean coordinator session: brief each, monitor, relay findings, clean up. Use when launching or supervising more than one agent at once, or when the coordinator context is getting crowded."
when: A task splits cleanly into parallel workstreams, the coordinator's context is getting crowded, or you need to launch, supervise, and reconcile multiple workers.
keywords: [multi-agent, coordination, agents, workers, parallel, monitor, relay, brief, handoff, cleanup]
---
1. Decide whether independent work warrants multiple agents. Keep short lookups, small
   edits, and one continuous trace local.
2. Read `bn skills render coordinate-agents-reference` for briefing, monitoring, messaging
   and cleanup. For implementation phase sequencing, also read
   `bn skills render subagent-orchestration`; neither is a new public workflow.
3. Use available agents and the user's requested launch system. Start with the smallest
   useful team; give each worker a bounded scope, evidence contract, permitted writes,
   dependencies and stopping condition.
4. Keep one owner for decisions and integration. Workers do not launch further agents
   without that owner's approval. Reconcile evidence rather than repeating search trails.
5. Verify the integrated result, record findings, and stop only owned workers that are
   no longer needed.
