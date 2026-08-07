---
name: meeting-prep-query
internal: true
description: WorkIQ query for a focused meeting preparation brief
arguments:
  - { name: subject, description: Meeting subject., required: true }
  - { name: start, description: Meeting start time when known., required: false }
---
Prepare me for my meeting "{{subject}}" today.

Start time: {{start}}

In a few short bullets, give its purpose or agenda, my role or what is expected of me, the most
relevant recent email or Teams thread, and any document I should skim. Include links where
available.
