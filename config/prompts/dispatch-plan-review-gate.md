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
=== PLAN-REVIEW GATE ===

Before implementing anything or opening a PR, register the plan and wait for approval:

1. Investigate, then write the plan as Markdown — approach, touched files, risks — directly into
   the bn-owned draft file `{{plan_file}}`, not into branch-note todo/done-when items.
2. Submit that exact file, fan out the reviewers, and pause:
   `bn review submit --repo {{repo}} --branch {{branch}} --session {{session_id}} --reviewers {{reviewers}} --quorum {{quorum}} --plan-file {{plan_file}} [--label ...] [--path ...]`
   (include every source label and planned touched path so sensitive-plan checks cannot be
   bypassed), then `bn review agents <id>` to spawn the {{reviewers}} fresh, independent,
   read-only critics.
3. Poll with `bn review status <id>`. Do not write code or open a PR while status is `pending`.
   `consensus: human-required` needs explicit human approval even in unattended mode;
   `consensus: no-consensus` must be resolved by a human.
4. `approved`: implement the plan. {{verification_requirement}} When the work is green, open the
   PR, run `bn ingest`, and report with `bn agent done --summary '<one line>' --pr <url>`.
5. `changes-requested`: read `bn review show <id>`, revise, resubmit the same review, and poll again.
6. `rejected`: stop without implementing and run
   `bn agent done --report-only --summary 'plan rejected'`.

=== END PLAN-REVIEW GATE ===
