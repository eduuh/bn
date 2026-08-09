---
name: m365-email-schema
internal: true
description: JSON normalization contract for actionable Outlook mail
---
A JSON array, most-urgent first, each element:

`{"id":"<stable slug from sender+subject>","title":"<sender> - <subject> (<=6-word gist)","link":"<URL to open the message, else empty>","urgent":<true only if clearly high-stakes>}`

Include only emails that need a reply or action from me.
