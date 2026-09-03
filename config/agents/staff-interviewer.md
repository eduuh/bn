---
name: staff-interviewer
requires_repo: bn
kind: command
on_calendar: "Mon..Fri *-*-* 16:30:00"
opt_in: true
run: "bn retro tick"
network: false
description: "The pull-only weekly Interviewer reminder: raise Friday's retro item, escalate once Monday, then stop."
---
Raises this week's retro reminder on Friday, escalates it once on Monday, then stops. It never
nags twice and it never starts the conversation — the person does that with `bn retro chat` when
they are ready, which is the whole point of a pull agent.
