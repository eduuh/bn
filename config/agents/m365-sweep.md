---
name: m365-sweep
requires_repo: bn
kind: command
on_calendar: "Mon..Fri *-*-* 14:55:00"
catch_up: true
network: true
description: "bn — sweep M365 (workIQ) for urgent emails, Teams mentions, commitments, owed replies"
---
Implemented by `config/jobs/m365-sweep.sh` (run as `bn run m365-sweep`).
