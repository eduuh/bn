# Console redesign prototype

A self-contained, interactive HTML prototype of the full bn-web shell against the `clay`
tokens — open `console-redesign.html` directly in a browser, no build step.

What it exercises, in one surface:

- **Console** as Queue · Focus · Act — triage without navigating; `j`/`k`, `⌘K`, per-item verbs
- **Fleet** with a clickable node strip that filters agents by machine
- **Nodes** — per-machine cards (vitals, heartbeat, pinned agents), including an unreachable state
- **Cron** — job list with failing jobs cross-linked to their blocked Console items
- **Cost / Insights** split, **Today / Goals / Repos**, and Notes / Skills / Handoff
- Inline editing throughout: prompts, notes, goals, cron schedules, per-repo harness
- Phone layout: bottom tab bar, single-column console (Act folds under Focus at tablet width)

Data is illustrative. Class strings here are *not* evidence about the shipping system —
for that, use `../cards/`. This file argues layout, IA, and interaction, not component truth.
