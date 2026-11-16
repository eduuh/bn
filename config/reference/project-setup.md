# Project and machine setup

## Repository procedure

1. Confirm Git and Workspan are installed. Use a trusted installer only when installation was
   requested; do not pipe an unreviewed download into a shell. Inspect `git rev-parse
   --show-toplevel`, `git branch --show-current`, and `git status --short`. Reuse an existing
   checkout; a requested clone goes through `workspan wt clone <url> [name]`. Do not convert an
   existing clone, create a repository, or switch branches silently. Report detached HEAD or
   missing Git context rather than inventing it.
2. Read existing agent instructions, setup docs, manifests, CI, lockfiles, and pinned toolchains.
   Inspect the remote locally, redacting embedded credentials. Do not assume GitHub, `main`, or
   that a warm pool directory's basename is the repository name.
3. Inspect `workspan task --repo . --json` and the repo reference index (`workspan ref --repo`).
   These do not require a session record. Preserve existing file/Forge task authority. Do not
   create or claim a task solely to initialize the repository.
4. Preserve shared configuration and storage paths. The historical `notes_root` name still
   identifies configuration, jobs, references, and retained task storage; it is not session
   memory. See `docs/config.md` in the trusted checkout. Before changing an unclassified repo's
   personal/work routing, ask for the correct store; do not silently put work material in a
   personal store. Preserve existing `[work].work_repos` and all unrelated settings.
5. A machine covers its cloned repositories unless `[machine].repos` narrows it. If this repo is
   excluded, propose adding only this repo through the machine procedure below. Declining that
   change does not authorize changing managed clones, worker placement, schedules, or flags.
6. Follow the required Entire setup reference. From the project run
   `workspan-setup-entire copilot` or `workspan-setup-entire claude` for the selected harness.
   Use both only when requested. A missing tool, unapproved private destination, incompatible
   CLI, or failed privacy check is a blocker. Do not enable capture with a default code-remote
   destination, import history, or push transcripts.
7. Inspect destination files, then run `workspan setup mcp init copilot` or
   `workspan setup mcp init claude`. The managed JSON merge and instruction markers preserve
   other servers and prose. Do not replace whole files or write machine-specific sockets.
   Copilot configures `.github/mcp.json`, `.vscode/mcp.json`, and
   `.github/copilot-instructions.md`; Claude configures `.mcp.json` and `CLAUDE.md`.
   The client spawns `workspan mcp serve` over stdio in its worktree. A missing launcher is a
   prerequisite failure, not a successful connection.
8. Do not opt into MCP extras automatically. Inspect `[mcp].extras`: configured extras may fetch
   packages on first launch even without `--with-extras`. Surface that and obtain permission for
   additions. Do not change global extras to work around one project's choice.
9. Preserve a working verification gate. Prefer native commands and pinned package managers.
   Use shared `<storage-root>/jobs/<repo>/build.sh` (plus meaningful `test.sh`/`lint.sh`) or
   committed `.bn/jobs/` scripts. For a single native gate, use `[repo.<name>].verify` while
   preserving other settings. Scripts run through `/bin/sh` from the worktree: keep them
   portable, idempotent, and independent of absolute checkout paths. Inspect resolution before
   execution so an unrelated global job cannot masquerade as this repo's check.
10. Run `workspan verify` for the selected complete gate and required standalone checks not
    covered by it. Install dependencies only after a manifest change or a missing-dependency
    failure. Do not add frameworks, upgrade packages, change product code, weaken checks, or
    invent unconditional-success scripts to manufacture green. For a docs-only repository with
    no gate, say so instead of claiming verification passed.
11. Run `entire status` and confirm the selected harness and approved private checkpoint
    destination. Rerun the selected MCP setup and inspect the diff for idempotence. Check
    `workspan skills find setup bn` and the generated native skill file; discovery alone does
    not prove native installation, and writing files does not prove a running editor reloaded.
    Record verified build facts in repo references with source and date. Put actionable blockers
    on an existing assigned task; Entire owns session history.

Report the repo/branch, harness files changed, verification gate or why none exists, capture
status, and blockers. State whether machine settings were unchanged. Newly wired MCP servers or
skills need an editor reload. Do not deploy, register services, or publish code as a setup probe.

## Optional graphify integration

Only when requested or approved, use the installed graphify CLI and upstream skills. Report a
missing tool rather than installing ad hoc. `graphify claude install` changes instructions and
Claude tool hooks; `graphify vscode install` changes Copilot instructions. Inspect existing files
first. `graphify hook install` additionally changes Git hooks and `.gitattributes`; keep
machine-specific hook paths uncommitted unless approved. Ignore `graphify-out/` unless the user
wants it committed. A first graph build spends model tokens; do not run it automatically.

## Machine-only procedure

Do not run repository onboarding when only machine configuration was requested.

1. Identify the machine from `workspan machine list --json` and the `<config_dir>/machine` pin
   (or `BN_HOSTNAME`). Read the shared and machine-specific `config.toml` files described in
   `docs/config.md`; show the exact paths before editing. A new machine with no history row is
   normal, not a reason to merge another machine's history.
2. Report the effective `[machine].repos` (default `["*"]`) and `.cron` (default `true`).
   These are not `[managed_repos]`, `[nodes.<id>].repos`, or per-job `only_on` restrictions.
3. Collect the requested repository allowlist and whether cron should be off, then show the
   current and proposed tables. Wait for explicit approval. Declining means no machine change.
   Prefer full `owner/repo` slugs, preserve owner identity, normalize duplicates and whitespace,
   and never shorten ambiguous same-basename repositories.
4. Change only this machine's file, preserving unrelated tables. Omit default-valued keys.
   `cron = false` removes only Workspan's own timers when reconciled; it does not authorize
   touching unrelated systemd, launchd, or Windows services.
5. Validate parsing with a read-only Workspan command, then run
   `workspan cron register --dry-run`. Confirm disallowed repos are absent. Obtain explicit
   confirmation before removals, then reconcile through `workspan cron register` and inspect
   `workspan cron`. Do not dispatch a task as a permissions probe.
6. If validation fails, restore only the table changed here. Report the exact failure and leave
   the last configuration that parsed successfully. Name the file changed and reconciliation
   result.
