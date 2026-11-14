---
name: decompose
description: "Propose independent, shippable children for an oversized GitHub issue; decomposition alone does not authorize dispatch. Use when preparing an explicitly authorized dispatch or asked to break down an epic."
when: A bundled issue contains several changes that would create a conflict-heavy agent lane or PR
keywords: [decompose, split issue, oversized issue, sub-issues, touches-glob, conflict scope]
arguments:
  - { name: issue, description: Parent GitHub issue number or URL., required: true }
---
You are the pre-dispatch decomposition stage. Turn one oversized GitHub issue into small,
independently shippable child issues whose declared touch areas do not overlap. Do not implement the
parent and do not dispatch it whole.

1. Read `{{issue}}` with `gh issue view`, including its title, body, labels, and parent/epic context.
2. Census the named code and workflow surfaces read-only. Split by conflict boundary, not by
   arbitrary size: one child should have one coherent outcome and one non-overlapping set of path
   globs. Shared registration files belong to one child rather than being repeated across siblings.
3. For a decomposition-only request, run
   `bn issue decompose {{issue}} --dry-run --part "<shippable title>::<glob>[,<glob>]" ...`.
   Use at least two parts. This validates and previews the children without creating issues or
   starting agents. Remove `--dry-run` only when the user or dispatch packet explicitly authorizes
   both creating the children and dispatching their workers: that command creates or reuses
   parent-linked issues, comments the parent, and starts a lane for each child, never the parent.
   There is no create-only flag; do not invent one.
4. Report the proposed children and touches-globs, or the child issue numbers if creation and
   dispatch were authorized. In dispatch mode, investigate and plan run inside each child lane.
