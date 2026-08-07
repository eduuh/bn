---
name: meetings-schema
internal: true
description: JSON normalization contract for today's meetings
---
Output ONLY a JSON array (no prose, no code fence), sorted most-important first
(critical > high > medium > low), each element:

`{"id":"<stable slug from time+subject>","start":"<HH:MM 24h, or empty>","subject":"<text>","joinUrl":"<the https://teams.microsoft.com/l/... link associated with THIS meeting, else empty>","importance":"critical|high|medium|low"}`

Meetings are usually followed by footnote links like
`[1](https://teams.microsoft.com/l/...)`, `[2](...)`. The text (a notes section) states which
footnote number belongs to which meeting. Use that mapping to attach each meeting its OWN link as
`joinUrl`; never reuse one link for a different meeting or invent a URL. Use only meetings present
in the text. Return an empty array if there are none.
