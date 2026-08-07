---
name: attention-completion
internal: true
description: Worktree attention execution and completion envelope
arguments:
  - { name: id, description: Attention item id., required: true }
  - { name: prompt, description: Attention task prompt., required: true }
---
First run `bn --cat` to read this branch's goal, open todos, and decisions.

Task:

{{prompt}}

When work is complete:

- If code changed, commit with a conventional message, push the branch, open a draft PR, then run
  `bn attn done {{id}} --link <pr-url> --notes "<one-line summary>"`.
- Otherwise run `bn attn done {{id}} --notes "<one-line summary>"`.

Do not run `bn attn ack`, do not merge the PR, and do not close the human review gate.
