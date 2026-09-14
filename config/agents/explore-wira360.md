---
name: explore-wira360
requires_repo: wira360
kind: command
on_calendar: "*-*-* 03:00:00"
network: true
retry_after: 2h
description: "wira360 — the explore pass: drive the local forge dev stack as a user and file reproduced product bugs as needs-triage (gated: a human decides before any fixer picks one up)"
---
Dispatches a scout seeded with the `explore` skill against wira360's **local** forge stack.

No `run:` line, deliberately: the agent resolves to the same-named job script,
`jobs/explore-wira360.sh`, which holds the lane's settings — including the `needs-triage` label the
gate below depends on. On a `run:` command line they were silently lost: the agent is fired by a
timer through the scheduler, not from your shell, so the script is the one place its settings are
always set.

**Why this one is gated.** wira360 is product-facing, so an autonomously-filed bug here is a claim
about a real product with real users. Two constraints follow, and both are load-bearing:

- **Local stack only.** `dev:forge:family` pins its own ports and a local seeded postgres. The
  explorer must never point at a deployed environment — a "reproduction" against shared data is
  not a reproduction, and the writes it performs while exploring are destructive there.
- **`needs-triage`, not the fix lane.** Findings file with an extra `needs-triage` label and are
  deliberately *not* pulled into the fix arm. You decide which become work. Drop the label (or
  add it to `bn task pull`'s filter) on the ones you want fixed, and the existing fix arm takes
  them from there.

The heavier stack is why this runs an hour after `explore-bn` rather than beside it.
