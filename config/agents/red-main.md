---
name: red-main
requires_repo: bn
kind: command
opt_in: true
on_calendar: "*:0/30"
network: true
description: "red main — every 30 minutes, a repo whose default branch went red gets one agent that reproduces and fixes it in a PR (autonomous; kill switch ~/.bn/red-main.off)"
---
Watches the default branch's CI on every repo you pushed to in the last 30 days. A red workflow
nobody is fixing gets one agent, seeded with the `red-main` skill and a checklist of the failing
runs. A green fleet costs one `gh run list` per repo. No `run:` line: the agent resolves to
`jobs/red-main.sh`, which holds the settings and guards.

Opt-in, for a worker machine only. On it, in its machine config.toml:

```toml
[agents]
enabled = ["red-main"]
```

Settings go in `~/.config/bn/red-main.env` there; see the script header.
