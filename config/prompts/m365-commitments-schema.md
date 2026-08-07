---
name: m365-commitments-schema
internal: true
description: JSON normalization contract for personal commitments
---
Output ONLY a JSON array (no prose, no code fence), each element:

`{"text":"<short imperative task>","link":"<source URL, else empty>"}`

Include only real commitments or requests that are mine; never invent one. Return an empty array
if there are none.
