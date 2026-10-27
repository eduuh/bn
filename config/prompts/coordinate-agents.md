---
name: coordinate-agents
description: "Run several work agents from one lean coordinator session: brief each, monitor, relay findings, clean up. Use when launching or supervising more than one agent at once, or when the coordinator context is getting crowded."
when: A task splits cleanly into parallel workstreams, the coordinator's context is getting crowded, or you need to launch, supervise, and reconcile multiple workers.
keywords: [multi-agent, coordination, agents, workers, parallel, monitor, relay, brief, handoff, cleanup]
---

# Coordinating many agents

These lessons come from running many agents in parallel on large tasks such as a monorepo-wide change or a telemetry investigation. The coordinator decides, relays, and monitors. Workers do the work.

Use Copilot fleet / Claude subagents for substantial research, independent reviews, or bulky
build/test output that would otherwise crowd the main context. Start with the smallest useful
team and expand only when another independent assignment can make progress. There is no fixed
agent-count cap. Keep one main owner for decisions, integration, shared writes, and final
verification. Short known-file lookups, small edits, and one continuous trace stay local.

## Roles

- **Coordinator:** keeps the thread short, sets the goal, briefs workers, watches for blockers, and asks the human only on genuine ambiguity. It does not turn every worker failure into a local debugging rabbit hole.
- **Workers:** one per independent workstream, launched visibly enough that a human can see status, interrupt, or take over if needed.
- **Relay:** a small shared note, board, or log of findings, decisions, blockers, and links. Keep it append-only when possible.

## Briefing workers

Each worker brief should say:

- the exact goal and done-when criteria
- where to work and what state already exists
- the hard constraints, especially what not to change
- how to report progress and blockers
- which other worker or decision it depends on, if any
- the exact revision, evidence required, and permitted writes
- a stopping condition and a concise result contract: findings with paths or sources, commands
  and exit codes, uncertainties, and artifact locations

Give each worker a bounded slice. A UI bug, a test gap, and a deployment concern are three briefs, not one broad one.

## Running the loop

1. Launch only the workers that can make progress in parallel.
2. Wait for evidence, not vibes.
3. Relay findings as decisions or next questions, not transcripts.
   Follow up with the same worker for the same task when its retained context helps. Start a
   fresh worker for unrelated work. Do not maintain unrelated worker pools, reread the worker's
   entire search trail, or repeat its checks without a concrete reason. Workers do not launch
   further agents unless the main owner explicitly authorizes an independent split.
4. If a worker is blocked, either narrow the task or ask the human.
5. When the work is done, close the loop: summarize what changed, what was verified, and what remains.

## Workers on other machines

A worker can run on a Cloud PC or another PC as easily as here: `bn agent launch claude --host <machine>` from the branch's worktree. It runs in a tmux window on that machine and survives dropped connections. `bn agent msg` reaches it, and it can answer back, whichever machine each side is on. `bn machine tunnels` shows which machines are up. The `multi-device` skill has the setup and the troubleshooting.

## Cleanup

- Stop or close worker views once their work is done.
- Record the final decision, the key evidence, and any follow-up that should survive the session.
- Keep the coordinator session lean so it can keep steering.

## Good fits

- A large repo task split across implementation, tests, docs, and review.
- Parallel investigations where each thread answers one concrete question.
- Example workloads such as a monorepo migration or a telemetry sweep.

## Bad fits

- A single short edit that one person can do faster than the overhead of coordination.
- Work that is already blocked on one missing decision or one external dependency.
