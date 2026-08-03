---
name: m365-sweep
on_calendar: "Mon..Fri *-*-* 14:55:00"
run: "bn run m365-sweep"
network: true
description: "bn — sweep M365 (workIQ) for urgent emails, Teams mentions, commitments, owed replies"
---
# m365-sweep

Sweeps M365 (via workIQ) for urgent emails, Teams mentions, commitments, and replies you
owe, folding them into the feed + attention list. Runs `bn run m365-sweep` on weekday
afternoons, just after `meetings`; the due-check catches a missed morning slot on the next
`info-gather` tick.
