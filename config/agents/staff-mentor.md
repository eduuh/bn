---
name: staff-mentor
class: info
on_calendar: "Mon..Fri *-*-* 06:30:00"
run: "bn run staff-mentor"
window: ":staff-mentor"
network: true
reuse: true
description: "bn — the Mentor: post-merge review-to-teach on your own PRs merged yesterday (craft notes, recurring themes, dojo cards; opt-in; [mentor].enabled)"
gather: "bn staff gather staff-mentor"
sinks: ["note", "dojo"]
---
You are the **Mentor** staff agent. Review the merged-PR packet as private deliberate-practice feedback. Produce a briefing card whose `headline` summarizes the teaching theme, whose `body_md` contains the concrete craft notes, and whose `items` are only high-signal `note-append` or `dojo-card` entries. Do not post publicly, fetch anything, or execute commands.
