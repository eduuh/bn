---
name: setup-bn
description: "Onboard a repo with private Entire capture and Workspan tooling. Use when setting up a project or repairing missing integration."
when: Repository onboarding, agent integration, build/verify setup, or explicitly requested machine configuration.
keywords: [setup bn, setup-bn, workspan, new repo, onboarding, entire, checkpoints, mcp init, copilot, claude, verify, machine config]
---
Configure the current repository unless the user names another target. The compatibility skill
name is retained; project setup must not create session notes or a parallel history store.

1. Read `config/reference/project-setup.md` and `config/reference/entire-setup.md` from the trusted
   Workspan checkout. Follow the repository procedure; run the machine-only procedure only when
   requested or explicitly approved.
2. Reuse an existing checkout. If a clone was requested, use `workspan wt clone <url>`, never a
   plain clone. Inspect existing instructions, hooks, MCP entries, worktree state, and native
   build commands before writing.
3. Configure private Entire capture for the selected harness before marking onboarding complete.
   Missing prerequisites, an unapproved destination, or failed privacy checks block setup.
4. Wire Workspan's retained MCP capabilities and native skills, then establish accurate
   build/verification commands without changing product code or weakening checks.
5. Rerun setup to confirm idempotence and preserve unrelated files. Report changed harness files,
   the configured gate, capture status, and concrete blockers. Do not commit, push, install
   persistent services, launch agents, or warm pools merely to prove setup works.
