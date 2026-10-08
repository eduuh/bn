---
name: dispatch-plan-work-bootstrap
internal: true
description: Bootstrap a fresh build-from-plan dispatch whose note has only a goal
---
The note starts with only the goal — there is no plan artifact yet, so build-from-plan's "no plan →
stop and recommend `plan`" case does not apply here: write the plan first, then execute it. Run
`bn plan init`, write `plans/active.md` as the Markdown execution contract (goal, 2-5 observable
done-when criteria, ordered steps, risks, open questions), then execute the build-from-plan loop from
it. Do not decompose the plan through repeated `bn add todo` / `bn add done-when` calls — keep
the branch note as the index/status layer.
