---
name: write-like-me
description: "Draft, rewrite, and polish requested writing in my voice. Use when I ask for a writing artifact, never for ordinary agent output."
when: I explicitly ask to draft, rewrite, or polish a writing artifact, including text for someone else or a task plan. Never for ordinary agent replies, progress updates, explanations, code output, completion summaries, or internal planning.
keywords: [rewrite, send, team, message, planning, task plan, writing, blog, post, draft, prose, voice, tone, style, polish, readme, email, de-ai, ai tells, em-dash, plain english]
arguments:
  - { name: topic, description: "What to write about, or the path of a draft to rewrite. Omit and I will ask.", required: false }
  - { name: kind, description: "rewrite | message | plan | blog | readme | pr | issue | email | notes. Use the requested format.", required: false }
---
Apply this skill only to an explicitly requested writing artifact, never to ordinary replies,
progress updates, explanations, code output, completion summaries, or internal planning.
Topic: {{topic}}. Kind: {{kind}} (use the requested format).

1. Render `bn skills render write-like-me-workflow`, passing topic and kind through `--arg`,
   and follow its detailed writing and checking steps.
2. Use verified facts and saved writing preferences; invent neither personal preferences
   nor measurements. Match the requested artifact's format.
3. Keep the writing rules confined to that artifact. Do not restyle the rest of the agent
   session or apply blog structure to short messages.
4. Report the artifact and any real uncertainty without a victory lap.
