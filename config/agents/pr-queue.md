---
name: pr-queue
requires_repo: bn
kind: command
on_calendar: "*:0/15"
network: true
run: "bn reviews --fetch"
description: "bn — fetch the PR review queue from GitHub (review + my own PRs)"
---
Refreshes the local review-queue cache from GitHub every 15 minutes: the open PRs awaiting your
review, and your own. Deterministic — it decides nothing, and `bn reviews` reads what it wrote.
