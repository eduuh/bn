---
name: meetings
class: info
on_calendar: "Mon..Fri *-*-* 14:50:00"
run: "bn run meetings"
window: ":meetings"
network: true
reuse: true
description: "bn — surface today's important meetings (workIQ) to the feed + attention list"
---
# meetings

Surfaces today's important meetings (from workIQ) into the feed and attention list on
weekday afternoons. Runs `bn run meetings`. `info-gather`'s due-check also fires it on the
next tick after a late start, so the old first-event-of-day catch-up is preserved without a
separate timer.
