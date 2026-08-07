---
name: m365-owed-schema
internal: true
description: JSON normalization contract for replies owed by the user
---
Output ONLY a JSON array (no prose, no code fence), longest-waiting first, each element:

`{"id":"<stable slug from who+topic>","who":"<name>","topic":"<text>","days":<integer>,"link":"<URL, else empty>"}`

Include only threads genuinely awaiting my reply. Return an empty array if there are none.
