---
name: dispatch-plan-review-gate
internal: true
description: Concrete plan-review gate protocol appended after a ship workflow
arguments:
  - { name: repo, description: Target repository., required: true }
  - { name: branch, description: Target branch., required: true }
  - { name: session_id, description: Dispatch session id., required: true }
  - { name: reviewers, description: Required independent reviewer count., required: true }
  - { name: quorum, description: Required approval quorum., required: true }
  - { name: plan_file, description: Markdown plan file to submit., required: true }
  - { name: verification_requirement, description: Optional post-approval verification clause., required: false }
---
=== PLAN-REVIEW GATE - READ THIS LAST; IT OVERRIDES EARLIER PR STEPS ===

Before implementing anything or running `gh pr create`, register the plan and wait for approval:

1. Investigate, then write the plan as Markdown with the approach, touched files, and risks in:
   `{{plan_file}}`
2. Submit it and pause:
   `bn review submit --repo {{repo}} --branch {{branch}} --session {{session_id}} --reviewers {{reviewers}} --quorum {{quorum}} --plan-file {{plan_file}} [--label ...] [--path ...]`
   Include every source label and planned touched path so sensitive-plan checks cannot be bypassed.
3. Run `bn review agents <id>` to fan the plan out to exactly {{reviewers}} fresh, independent,
   read-only critics.
4. Poll with `bn review status <id>`. Do not write code or open a PR while status is `pending`.
   `consensus: human-required` needs explicit human approval even in unattended mode;
   `consensus: no-consensus` must be resolved by a human.
5. `approved`: implement the plan. {{verification_requirement}} Then open the PR, run `bn ingest`,
   and report with `bn agent done --summary '<one line>' --pr <url>`.
6. `changes-requested`: read `bn review show <id>`, revise, resubmit the same review, and poll again.
7. `rejected`: stop without implementing and run
   `bn agent done --report-only --summary 'plan rejected'`.

=== END PLAN-REVIEW GATE ===
