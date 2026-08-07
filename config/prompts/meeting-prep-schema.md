---
name: meeting-prep-schema
internal: true
description: JSON normalization contract for a meeting preparation brief
---
Output ONLY a JSON array (no prose, no code fence) of 2-5 short prep bullets, each:

`{"id":"<stable slug>","text":"<one short line>","link":"<URL or empty string>"}`

Ground every bullet in real context from the data; never invent a fact or URL. Return an empty
array when there is nothing useful to say.
