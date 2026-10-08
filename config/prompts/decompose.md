---
name: decompose
description: "Split an oversized GitHub issue into independent, shippable child issues. Use before dispatching an issue that is too big for one PR, or when asked to break down an epic."
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
3. Run `bn issue decompose {{issue}} --part "<shippable title>::<glob>[,<glob>]" ...`.
   Use at least two parts. The command validates that sibling globs do not overlap, creates or
   reuses parent-linked child issues, comments the parent with their checklist, and starts work on the
   children only, never the parent.
4. Report the child issue numbers and their touches-globs. The parent remains a tracking bundle;
   investigate and plan run independently inside each child lane.
