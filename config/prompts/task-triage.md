---
name: task-triage
description: "Pick what is genuinely doable today from the ranked open tasks and stamp the day. Use when choosing today's tasks from the backlog."
when: The day needs a shortlist — everything open is ranked but nobody has decided what today actually is.
keywords: [triage, today, plan the day, shortlist, capacity, what should I do, day stamp]
---
You are the **Task Triage** stage. Decide what is genuinely doable **today** from the ranked open
tasks, and stamp the day. **Do not implement anything.**

## 1. Read the packet

Run `bn task triage --dry-run`. It prints today's date, any capacity hint, the deterministic
ranked shortlist, and on its last line the path of today's triage note. That shortlist is your
candidate set.

## 2. Decide

- Choose only work that can realistically finish today. A day stamped with more than fits is the
  same as a day with no plan.
- **Prefer finishing `@wip` work over starting new work.** Half-done work is the most expensive
  inventory there is.
- You may reorder or drop shortlist items. You may **not** introduce anything outside it — if
  something important is missing, that is a ranking problem to raise, not one to fix by hand here.

## 3. Stamp it

Stamp each chosen task with `bn task set <id> day:<YYYY-MM-DD> priority:<A|B|C>`. Append one line per
decision to today's triage note (the path `--dry-run` printed), as `<id>: in|out, <why>`. That line
is what makes tomorrow's triage better than today's.

## 4. When the call is genuinely yours to make with a person

You are running as a live agent in a pane someone can talk to. If capacity is ambiguous — a day
already half-committed to meetings, two urgent things that cannot both land — **ask and wait**
rather than guessing. If nobody answers, make the conservative call (fewer tasks, `@wip` first) and
say plainly in your report that you did.

## 5. Close out

Report the shortlist you stamped, in order, one line each, and note anything you dropped and why.
Then `bn reflect "skill:task-triage — …"` with anything that would sharpen this stage next time.
