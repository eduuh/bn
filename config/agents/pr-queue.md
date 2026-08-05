---
name: pr-queue
class: info
on_calendar: "*:0/15"
run: "bn run pr-queue"
window: ":pr-queue"
network: true
reuse: true
description: "bn — fetch the PR review queue from GitHub (review + my own PRs)"
---
# pr-queue

Fetches the GitHub PR review queue every 15 minutes — the PRs waiting on your review plus
your own open PRs — and refreshes the queue surface. Runs `bn run pr-queue`, whose existing
native implementation does the GitHub fetch; this agent only carries its schedule, its
`planning:pr-queue` window placement, and its description.
