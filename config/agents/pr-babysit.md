---
name: pr-babysit
requires_repo: bn
kind: command
opt_in: true
on_calendar: "*-*-* 04:00:00"
network: true
retry_after: 2h
description: "pr babysit — nightly, your own open PRs: merge main into conflicting ones, fix red CI, report which are ready (never merges; kill switch ~/.bn/pr-babysit.off)"
---
Sorts every open PR you authored into ready, conflicting, red, pending, draft or recently touched.
The ready ones are reported to you; each repo with conflicting or red PRs gets one agent, seeded
with the `pr-babysit` skill and a checklist of those PRs. Merging stays yours. No `run:` line: the
agent resolves to `jobs/pr-babysit.sh`, which holds the settings and guards.

Opt-in, for a worker machine only. On it, in its machine config.toml:

```toml
[agents]
enabled = ["pr-babysit"]
```

Settings go in `~/.config/bn/pr-babysit.env` there; see the script header.
