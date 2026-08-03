---
name: task-improve
on_calendar: "*-*-* 0/1:00:00"
run: "bn task improve --due"
network: true
description: "bn — task improve: make the highest-ranked unscoped task SMART (why, done-when, estimate, day) so the gate will admit it"
---
# task-improve

Every hour, takes the highest-ranked task that has **not** passed the SMART gate and makes it
executable: fills in `## Why` and `## Done When` in the task's context doc, sets an estimate and a
day, then lets the gate re-judge it.

This is the other half of `bn task pull`. A pulled issue is a title, not an acceptance criterion —
it deliberately carries no `smart:` verdict, so the gate (#998) routes it here before any executor
sees it. Without this agent the pulled backlog would simply pile up unscoped.

Skips tasks marked `smart:stuck`: three attempts already failed, so those are waiting on a person,
not on a fourth try.

Run it by hand on a specific task with `bn task improve <id> --steer "things to look out for"`,
or `--dry-run` to read the seed without launching anything.
