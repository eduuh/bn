---
name: notes-commit
description: One-line conventional commit subject for staged notes changes.
when: Committing staged changes in a personal notes repo and you need the commit subject.
keywords: [commit, commit message, notes, conventional commit, git, subject line]
---
The staged changes to a personal notes repo are provided on stdin. Write ONE concise
git commit message line in conventional style, starting with "notes: ", summarizing
what changed. Output only that single line — no quotes, no body, no markdown.
