---
name: meetings-query
internal: true
description: WorkIQ query for today's full meeting list
arguments:
  - { name: weekday, description: Local weekday., required: true }
  - { name: date, description: Local YYYY-MM-DD date., required: true }
---
List ALL my Outlook calendar meetings for today, {{weekday}} {{date}}; do not omit any. For each,
give the start time (HH:MM, 24-hour), the subject, the Teams join link if present, and rate how
important it is for me to attend as one word: critical, high, medium, or low. Use low for optional,
FYI/broadcast, or recurring meetings where I am not a required attendee. One line per meeting.
