# Entire during project setup

Entire session capture is mandatory for repository onboarding, not an optional extra.
Configure only the selected harness; machine-only setup does not enable projects.

1. Confirm the target is an existing Git worktree and inspect existing hooks/settings.
   If Entire is missing, install it using the official installation documentation and a
   reviewed installer or package-manager command. Do not pipe unreviewed downloads into a shell.
   Verify the installed `entire enable --help` supports the flags in the helper.
2. Resolve the explicitly approved private checkpoint destination from
   `BN_ENTIRE_CHECKPOINT_REMOTE` or `${XDG_CONFIG_HOME:-$HOME/.config}/bn/entire-checkpoint-remote`.
   The file contains one line: `github:owner/repo`. This is personal machine configuration, never
   a destination hard-coded into a shared skill or committed project settings.
   If absent, ask once for a destination and permission to persist it. Never guess, create a
   repository, reuse a destination with different access requirements, or upload to code's remote.
3. From the project, run `bn-setup-entire copilot` (use `claude` for Claude Code).
   `install.sh` installs this helper; before installation use
   `bash <trusted-bn-checkout>/config/workflow/bin/setup-entire.sh copilot`.
   The helper verifies private visibility through `gh`, enables
   capture non-interactively with the destination in local settings, then runs `entire status`.
   No history import, login, hosting/mirroring, push, or force hook replacement is performed.
4. Inspect the effective destination in `entire status` and ensure existing hooks remain.
   Repeat setup to check idempotence. Missing tools, failed privacy checks, incompatible Entire
   versions, or setup failures are blockers: do not mark onboarding complete or fall back to
   uploading transcripts alongside code.

Personal checkpoint settings must stay local even for forks or projects under another owner:
Entire can ignore an inherited project-wide destination and otherwise upload alongside code.
Do not enable capture before selecting the private destination. Do not migrate previously
captured sessions or bulk-enable existing projects as a side effect of onboarding.

Sources:
- https://docs.entire.com/installation
- https://docs.entire.com/cli-reference/enable
- https://docs.entire.com/guides/checkpoints/store-checkpoints-in-another-repo
