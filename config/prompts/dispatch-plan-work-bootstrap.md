---
name: dispatch-plan-work-bootstrap
internal: true
description: Bootstrap a fresh plan-work dispatch whose note has only a goal
---
The note starts with only the goal and has no plan artifact yet. First run `bn plan init`, then
write `plans/active.md` as the Markdown execution contract: goal, 2-5 observable done-when
criteria, ordered steps, risks, and open questions. Do not decompose the plan through repeated
`bn add todo` / `bn add done-when` calls. Keep the branch note as the index/status layer, then
execute the plan-work loop from `plans/active.md`.
