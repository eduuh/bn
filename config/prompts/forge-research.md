---
name: forge-research
description: Complete a Forge research task with sourced findings and a rich Markdown task update.
when: A Forge-assigned task asks for research, comparison, selection, triage, or a recommendation without requiring a repository code change.
keywords: [forge, research, recommendation, compare, findings, sourced, rich markdown, images, markdown summary, no code change, agent update]
arguments:
  - { name: question, description: Research question or task goal., required: false }
---
Use this lane when the Forge task is asking for knowledge, a comparison, or a recommendation.

1. Restate the research question ({{question}}) in one line in the branch note.
2. Gather evidence from the task context, existing Forge goal/activity data, repo notes, and web or
   local sources only when needed. Keep citations or source names with each material fact.
3. Compare options against the criteria in the Forge task. Prefer a small decision table when there
   are multiple choices.
4. Choose one recommendation when the task asks for one. If a required fact is missing and cannot be
   found, name the missing fact and the best reversible next step.
5. Finish Forge with rich Markdown. Use headings, tables, links, checklists, and images when they
   improve the report. For images, prefer stable public/source URLs or generated artifacts already
   attached/hosted by the app; always include useful alt text.
   - `## Recommendation`
   - `## Key findings`
   - `## Comparison`
   - `## Evidence checked`
   - `## Remaining risks / next step`

If no repo files changed, do not create a PR just to report research. Update Forge and the branch
note instead.
