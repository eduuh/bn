---
name: explore-bn
requires_repo: bn
kind: command
on_calendar: "*-*-* 02:00:00"
network: true
retry_after: 2h
description: "bn — the explore pass: drive the running bn-web dashboard and CLI as a user, reduce what breaks to a deterministic repro, and file only what reproduces (autonomous; kill switch ~/.bn/explore.off)"
---
Dispatches a scout seeded with the `explore` skill. The skill is the process; this manifest is the
schedule and the blast radius.

No `run:` line, deliberately: the agent resolves to the same-named job script, `jobs/explore-bn.sh`,
which holds the lane's settings. The agent is fired by a timer through the scheduler, never from
your shell, so nothing from your environment reaches it; settings in the script are the only ones
that are always set, and they sit in git where they can be reviewed. They were on a `run:` command
line at first, and the driver ran with nothing set and exited 2.

**Why the focus line is narrow.** bn's dashboard drives the machine's *real* fleet — the agents,
crons and worktrees actually running here. Unlike a product app there is no throwaway instance to
point at, so the safety boundary is the action, not the environment: read every surface, exercise
the safe and self-reverting writes, and stop at anything that steers, relaunches, kills or lands.
The stranded prototype this skill came from proved the highest-value bugs live in exactly that
control path — and it found them by *reading* the responses those actions returned, not by firing
them at live agents.

Fully autonomous by decision: findings file straight to the default fix lane, and the fix arm
dispatches fixers without asking. Merging still stays a human act.
