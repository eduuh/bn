---
name: dep-prs
requires_repo: bn
kind: command
opt_in: true
on_calendar: "*-*-* 03:00:00"
network: true
retry_after: 2h
description: "dependency PRs — land Dependabot/Renovate PRs across every repo you own: merge green patch/minor, fix red, leave majors (autonomous; kill switch ~/.bn/dep-prs.off)"
---
Dispatches one agent per repo with open dependency PRs, seeded with the `dep-prs` skill. The skill
is the process; this manifest is the schedule. No `run:` line: the agent resolves to
`jobs/dep-prs.sh`, which holds the settings and the guards.

Opt-in, so it runs only on the machine that turns it on (a worker, never the laptop at 3am).
`requires_repo: bn` is only the gate that the machine runs bn's own lanes; the job sweeps every repo.
On the worker, in config.toml:

```toml
[agents]
enabled = ["dep-prs"]
```

Settings for that machine go in `~/.config/bn/dep-prs.env` there (`BN_DEP_REPORT_TO`,
`BN_DEP_MAX`, `BN_DEP_AGENT`); see the script header.
