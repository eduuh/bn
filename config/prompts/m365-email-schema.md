---
name: m365-email-schema
internal: true
description: JSON normalization contract for actionable Outlook mail
---
Output ONLY a JSON array (no prose, no code fence), most-urgent first, each element:

`{"id":"<stable slug from sender+subject>","title":"<sender> - <subject> (<=6-word gist)","link":"<URL to open the message, else empty>","urgent":<true only if clearly high-stakes>}`

Include only emails that need a reply or action from me; never invent one. Return an empty array
if there are none.
