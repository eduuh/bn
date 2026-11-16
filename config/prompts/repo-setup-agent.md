---
name: repo-setup-agent
internal: true
description: "Configure accurate build and verification commands. Use for an assigned repository setup task."
arguments:
  - { name: repo, description: Target repository name., required: true }
---
Configure build/verification for `{{repo}}`, not product code.

1. Read manifests, CI, pinned toolchains, existing scripts, and repo references before choosing
   commands. Preserve working gates and unrelated settings.
2. Use shared `<storage-root>/jobs/<repo>/` scripts or committed `.bn/jobs/`. Keep `/bin/sh`
   scripts portable and idempotent. Add standalone test/lint scripts only when real commands
   exist; `[repo.<name>].verify` can select an existing complete native gate.
3. Exercise the smallest relevant entry point, then `workspan verify`. Do not upgrade packages,
   invent empty checks, change product code, or silently install missing tooling to get green.
4. Record verified build facts and misleading failures in repo references with source and date.
   Put actionable blockers and necessary decisions on the assigned task/provider. Entire
   captures session history; do not create a parallel setup ledger.
5. Report the gate, its evidence, and concrete blockers. A required product fix needs a separately
   authorized task. Repository onboarding also requires private Entire setup; follow
   `config/reference/project-setup.md` when that is part of the assignment.
