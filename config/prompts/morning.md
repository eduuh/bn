---
name: morning
description: Start-of-day sweep — sprint priorities, real review asks, blockers, CI/cron → today's 1–3
when: Beginning your day, before picking a branch — the 15–30 min morning routine, across all branches.
keywords: [morning, start of day, standup, sweep, priorities, inbox, review queue, blockers, ci, what now, plan my day]
---
You are running the user's start-of-day routine. This is **cross-branch** (the whole day's
picture), not a single branch — that's what `start-session` is for. Be fast and scannable;
the goal is to end with 1–3 concrete things to do, not a wall of text.

1. **Read the queue.** Run `bn next --json` in the shell. It is the distilled, priority-ranked
   morning signal — failing crons, real review asks, your stale PRs, today's goals, and blockers
   are already filtered and ordered, so read it instead of re-deriving them from raw sources.

2. **Interpret, don't relist.** For each pressing item, say what it means and name the single
   next action: a failing cron is a symptom to diagnose, not a chore to rerun; an over-SLA
   review ask means someone may be blocked — call those out first; for each `blocked` item,
   name the one missing thing (who/what) it's waiting on, since the habit is to raise these early.

3. **Land the plan.** Propose the day's **1–3** highest-leverage actions, heaviest first, each one
   line. Then offer to commit them with `bn daily add` so the day has a written target. Don't add
   them silently — the daily list is the user's plan.

End with: the 1–3, and the single first action to take now. Keep the whole thing under ~12 lines.
