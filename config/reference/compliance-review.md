---
name: compliance-review
description: "Design Doc compliance and security validation with optional auto-fixes. Use after implementation when a Design Doc exists, or when 'compliance-review' is mentioned. Note: requires docs/design/*.md to be present."
when: Design Doc compliance and security validation with optional auto-fixes.
keywords: [compliance-review, compliance, review, design, doc, security, validation, optional, auto-fixes, implementation, exists, mentioned]
---
You are the orchestrator: register Steps 1-11 using TaskCreate before any execution, then run each review, fix and check step as a `general-purpose` subagent (Agent tool) and pass their structured JSON between steps.

Design Doc (uses most recent if omitted): $ARGUMENTS

## Execution Flow

### Step 1: Prerequisite Check
```bash
# Identify Design Doc (most recently modified)
ls -t docs/design/*.md | grep -v template | head -1

# Check implementation files
git diff --name-only main...HEAD
```

If no Design Doc exists, report to user:
```
No Design Doc found at docs/design/*.md.
compliance-review requires a Design Doc for compliance checking.
Consider using /run-task for general code quality checks instead.
```

### Step 2: Code compliance review (code-reviewer)
- `subagent_type`: "general-purpose"
- `description`: "Code compliance review"
- `prompt`: "You are a code reviewer checking an implementation against its Design Doc. Design Doc: [path]. Implementation files: [git diff file list]. Review mode: full. Return JSON with complianceRate (%), verdict, identifierMatchRate, acceptanceCriteria (each fulfilled/partially_fulfilled/unfulfilled with gap and suggestion), identifierMismatches, and qualityFindings."

**Store output as**: `$STEP_2_OUTPUT`

### Step 3: Security review (security-reviewer)
- `subagent_type`: "general-purpose"
- `description`: "Security review"
- `prompt`: "You are a security reviewer. Design Doc: [path]. Implementation files: [git diff file list]. Review security compliance. Return JSON with status (approved | approved_with_notes | needs_revision | blocked, blocked only for committed secrets or equally severe issues), findings tagged confirmed_risk/defense_gap/hardening/policy with location, description and rationale, requiredFixes, and notes."

**Store output as**: `$STEP_3_OUTPUT`

### Step 4: Verdict and Response

**If security-reviewer returned `blocked`**: Stop immediately. Report the blocked finding and escalate to user. Do not proceed to fix steps.

**Code compliance criteria (considering project stage)**:
- Prototype: Pass at 70%+
- Production: 90%+ recommended

**Security criteria**:
- `approved` or `approved_with_notes` → Pass
- `needs_revision` → Fail

**Report both results independently using subagent output fields only**:

```
Code Compliance: [complianceRate from code-reviewer]
  Verdict: [verdict from code-reviewer]
  Identifier Match Rate: [identifierMatchRate from code-reviewer]
  Acceptance Criteria:
  - [fulfilled] [item] (confidence: [high/medium/low])
  - [partially_fulfilled] [item]: [gap] — [suggestion]
  - [unfulfilled] [item]: [gap] — [suggestion]
  Identifier Mismatches:
  - [identifier]: DD=[designDocValue] Code=[codeValue] at [location]
  Quality Findings:
  - [category] [location]: [description] — [rationale]

Security Review: [status from security-reviewer]
  Findings by category:
  - [confirmed_risk] [location]: [description] — [rationale]
  - [defense_gap] [location]: [description] — [rationale]
  - [hardening] [location]: [description] — [rationale]
  - [policy] [location]: [description] — [rationale]
  Notes: [notes from security-reviewer, if present]

Execute fixes? (y/n):
```

If the user selects `n`: Skip Steps 5-10, proceed to Step 11 (list any failing items under Remaining issues).

### Step 5: Load Task File Criteria

Execute Skill: documentation-criteria (its Work Plan **Includes** list and Three Elements of Task Completion define what the task file must hold; no template ships)

### Step 6: Create Task File

Create task file at `docs/plans/tasks/review-fixes-YYYYMMDD.md`
Include both code compliance issues and security requiredFixes.

### Step 7: Execute Fixes (task-executor)
- `subagent_type`: "general-purpose"
- `description`: "Execute review fixes"
- `prompt`: "You are a task executor. Task file: docs/plans/tasks/review-fixes-YYYYMMDD.md. Apply the fixes in order, stopping after 5 files changed. Return the changed files and any fixes not applied."

### Step 8: Quality Check (quality-fixer)
- `subagent_type`: "general-purpose"
- `description`: "Quality gate check"
- `prompt`: "You are a quality fixer. Run this repo's build, test and lint commands on the fixed files: [file list]. Fix failures you introduced. Return pass/fail per check."

### Step 9: Re-validate Compliance (code-reviewer)
- `subagent_type`: "general-purpose"
- `description`: "Re-validate compliance"
- `prompt`: "You are a code reviewer. Re-validate Design Doc compliance after fixes. Design Doc: [path]. Prior compliance issues: $STEP_2_OUTPUT. Verify each prior issue is resolved. Return the same JSON shape as before."

### Step 10: Re-validate Security (security-reviewer, only if security fixes were applied)
- `subagent_type`: "general-purpose"
- `description`: "Re-validate security"
- `prompt`: "You are a security reviewer. Re-validate security after fixes. Prior findings: $STEP_3_OUTPUT. Design Doc: [path]. Implementation files: [file list]. Return the same JSON shape as before."

### Step 11: Final Report

```
Code Compliance:
  Initial: [X]%
  Final: [Y]% (if fixes executed)

Security Review:
  Initial: [status]
  Final: [status] (if fixes executed)
  Notes: [notes from approved_with_notes, if any]

Remaining issues:
- [items requiring manual intervention]
```

## Auto-fixable Items
- Simple unimplemented acceptance criteria
- Error handling additions
- Contract definition fixes
- Function splitting (length/complexity improvements)
- Security confirmed_risk and defense_gap fixes (input validation, auth checks, output encoding)

## Non-fixable Items
- Fundamental business logic changes
- Architecture-level modifications
- Design Doc deficiencies
- Committed secrets (blocked → human intervention)

**Scope**: Design Doc compliance validation, security review, and auto-fixes.
