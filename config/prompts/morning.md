---
name: morning
description: Start-of-day sweep — sprint priorities, real review asks, blockers, CI/cron → today's 1–3
when: Beginning your day, before picking a branch — the 15–30 min morning routine, across all branches.
keywords: [morning, start of day, standup, sweep, priorities, inbox, review queue, blockers, ci, what now, plan my day]
---
You are running the user's start-of-day routine. This is **cross-branch** (the whole day's
picture), not a single branch — that's what `start-session` is for. Be fast and scannable;
the goal is to end with 1–3 concrete things to do, not a wall of text.

1. **The ranked queue.** Call `bn_next` (JSON). It already folds failing crons, attention items,
   PRs awaiting your review, your stale in-flight PRs, today's goals, and blocked notes into one
   priority-ordered list. Read it first — it is the distilled morning signal.

2. **CI / crons owned by you.** Call `bn_cron_status`. Flag only jobs whose **last** run failed
   (`ok: false`) — historical failure counts on a currently-green job are noise, say so. A broken
   scheduled job is a symptom to diagnose, not a chore to rerun.

3. **PRs waiting on *you*.** Call `bn_reviews`. Separate honestly:
   - `tag: review` **and author is not you** → a real review ask; someone may be blocked. These are
     the ones the routine means by "review 1–3 promptly."
   - authored by you → your own in-flight PR (a `pr`/nudge item, not something to review).
   - `tag: learning` → reading, not a review obligation.
   Call out over-SLA review asks first. Don't pad the count with your own or learning PRs.

4. **Blockers.** From `bn_next`/`bn_doctor`, surface anything `blocked` — and for each, the one
   missing thing it's waiting on. The SWE II habit is to raise these early, so name who/what each
   needs.

5. **Land the plan.** Propose the day's **1–3** highest-leverage actions, heaviest first, each one
   line. Then offer to commit them with `bn_daily_add` so the day has a written target. Don't add
   them silently — the daily list is the user's plan.

End with: the 1–3, and the single first action to take now. Keep the whole thing under ~12 lines.
