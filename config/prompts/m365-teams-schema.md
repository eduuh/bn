---
name: m365-teams-schema
internal: true
description: JSON normalization contract for unanswered Teams messages
---
Output ONLY a JSON array (no prose, no code fence), most-urgent first, each element:

`{"id":"<stable slug from who+gist>","title":"<who> (<where>): <=8-word gist","link":"<URL to open it, else empty>","urgent":<true only if clearly high-stakes>}`

Include only messages awaiting my reply; never invent one. Return an empty array if there are
none.
