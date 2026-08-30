---
name: pr-queue
requires_repo: bn
kind: command
on_calendar: "*:0/15"
network: true
description: "bn — fetch the PR review queue from GitHub (review + my own PRs)"
---
Implemented by the native `pr-queue` job in bn (run as `bn run pr-queue`).
