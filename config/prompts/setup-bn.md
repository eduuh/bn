---
name: setup-bn
description: Set up bn in a new repository with branch notes, agent integration, and build/verification commands; configure machine access and runtimes only with approval
when: Set up or onboard a new repo to bn, wire Copilot or Claude to its branch notes, configure its build/test/lint gate, or explicitly configure bn on a new computer.
keywords: [setup bn, setup-bn, configure bn, new repo, new repository, repo setup, onboard repository, initialize bn, branch notes, mcp init, copilot, claude, build scripts, verification, register machine, register repos, machine profile, enable queue, enable cron, enable web]
---

You are the **bn repository setup** workflow. Make the target repository usable with bn: resolve
its branch notes, wire the agent's MCP integration, and establish accurate build and verification
commands. Work in the current repository unless the user names another path or remote.

Choose the scope from the request:

- **New repository (default):** follow Repository setup below. Do not run the machine workflow
  unconditionally.
- **Machine configuration only:** skip repository setup and follow Machine configuration below.
- **Both:** onboard the repository and use Machine configuration for the explicitly approved
  access/runtime changes.

Preserve existing notes, user edits, MCP servers, instructions, and unrelated configuration.
Rerunning this skill must not duplicate entries, reset goals, or replace working scripts.
Do not commit, push, launch an agent, warm a pool, or install persistent services just to prove
setup works.

**Before running bn commands:** loading configuration can reconcile managed queue and web
services, even on a status command. Existing runtime policy remains in effect; leaving its flags
unchanged is not a promise that no service will start or stop. Warn before the first such command.
Use `tmux-run` for mutations and long-running commands when tmux is available.

## Repository setup

### 1. Inspect the target and prerequisites

Confirm `git` and `bn` are available (`git --version`, `bn --version`). If bn is missing, report
the prerequisite and use the documented installer from a trusted bn checkout only when installation
is requested. Do not invent a package-manager command or pipe an unreviewed download into a shell.

From the target checkout, inspect:

```bash
git rev-parse --show-toplevel
git branch --show-current
git status --short
```

Read the repository's existing agent instructions and setup documentation. Inspect the Git remote
locally, redacting any embedded credentials before displaying or recording its identity. Identify
the actual default branch, build system, package-manager lockfile, and pinned toolchain. Do not
assume the branch is `main`, the repository is on GitHub, or a directory named `_pool/9` is the repo
name.

Reuse an existing checkout. If the user supplied only a remote and requested a clone, first inspect
the configured layout in step 2, then use `bn wt clone <remote-url> [name]` and continue in the
worktree it reports. Check for an existing matching clone before doing so. Do not move or convert
an existing clone into bn's bare/worktree layout without approval. Outside Git, or on detached HEAD,
report the missing repository/branch context rather than creating a repository or switching branches
silently.

### 2. Resolve configuration and note storage

Run `bn setup --json`. Read the effective `config_path` it reports before editing anything, along
with any relevant environment overrides. This scaffolds missing configuration, refreshes the legacy
work-repo list, and installs native skills; it does **not** register a repository for dispatch.
Surface `skill_sync_failures` rather than interpreting an overall successful response as every
harness having been installed.

Resolve the exact machine key using the precedence in Machine configuration, step 1. Inspect the
current profile without changing it. Missing repositories and runtime flags are denied.

Establish whether the repository's notes belong in the personal or work store **before writing
notes**. Preserve an existing classification. For an unclassified repo, use an explicit user
instruction; otherwise ask which store to use, showing the resolved roots. Do not silently put work
material in personal notes. Work classification uses the normalized local repo name in
`[work].work_repos`, not the GitHub `owner/repo` authorization slug. Preserve other entries and
respect `BN_*` overrides; changing TOML does not override the environment. After an approved
classification change, rerun `bn setup --json` to refresh the legacy work-repo file.

If machine access is requested or missing, propose adding **only this repository** through Machine
configuration below. Preserve all existing grants and runtime flags by default; a missing profile
starts with all three runtime flags false. Use a full `owner/repo` for GitHub remotes. Do not invent
a GitHub slug for another provider; confirm the supported identity before granting access.
If approval is declined, continue the repo-local setup and explicitly report that machine
authorization/dispatch remains unconfigured. Do not touch `[managed_repos]`, worker placement,
cron schedules, or runtime flags as a side effect of onboarding.

### 3. Initialize the branch note without replacing history

From the target worktree, run:

```bash
bn status --json
bn --path
```

Confirm bn resolved the intended repository, branch, and note store. bn normalizes repo names to
lowercase and branch slashes to dashes; use its resolved identity, not a guessed filesystem path.
If resolution is wrong, stop before writing a note and explain the mismatch.

If there is no goal, set `bn goal "Set up bn for this repository"`. Leave an existing goal alone.
Record a setup todo with `bn add todo --if-absent setup-bn "setup-bn: wire branch notes and repository verification"`,
and record real decisions/blockers as they occur. Read the repository reference index with
`bn ref --repo` before rediscovering commands.

Use the target's CLI if the current session's MCP server belongs to another worktree. Branch-note
MCP tools are scoped to their launch directory; changing the shell cwd does not retarget them.

### 4. Wire the agent integration

Use the user's requested harness, otherwise the current harness (Copilot if no harness can be
identified). Configure both only when requested. Inspect the destination files first, then run the
applicable command from the repository root:

```bash
bn mcp init copilot
bn mcp init claude
```

Copilot writes `.github/mcp.json`, `.vscode/mcp.json`, and the managed block in
`.github/copilot-instructions.md`. Claude writes `.mcp.json` and the managed block in `CLAUDE.md`.
Use the command's JSON merge and instruction markers; do not replace entire existing files or
hand-write a machine-specific MCP socket/HTTP URL.

Do not pass `--with-extras` by default. Inspect `[mcp].extras` first: configured extras are included
even without the flag and may fetch packages on first launch. Surface that before wiring them and
obtain approval for additions beyond bn. Do not change the global extras list to work around a
repo-local choice.

The client starts `bn mcp serve` over stdio in its worktree; bn-web is not required. Do not start
`bn serve --daemon` merely to make branch-note MCP work. Report missing runners or a missing
`bn-mcp` binary as prerequisites, not a successful connection.

### 5. Make build and verification commands accurate

Read manifests, CI workflows, existing scripts, and repo references. Reuse native commands and
pinned package managers. Do not add test/lint frameworks, upgrade dependencies, or change product
code to manufacture a green setup. Install dependencies only when the chosen command requires
missing dependencies, following the repository's documented procedure.

Inspect existing bn job scripts and `[repo.<resolved-name>].verify` before choosing a gate.
Preserve a working gate. For missing configuration:

- Prefer machine-independent repo scripts at `<resolved-notes-root>/jobs/<resolved-repo>/build.sh`,
  with `test.sh` and `lint.sh` only when real standalone commands exist. These are **not** inside a
  machine's `main` branch-note directory.
- Use committed `.bn/jobs/<name>.sh` when shared project configuration is wanted, but confirm bn
  actually resolves it in this checkout. Project-job discovery uses the conventional worktree
  layout; arbitrary clones and pooled paths must not be assumed to resolve that tier. Use the
  notes-repo location when they do not.
- If one existing native command is the correct complete gate, set
  `[repo.<resolved-name>].verify` to that command, preserving other repo settings. This overrides
  the default build/test/lint sequence; it must not silently omit a required check.

Scripts run through `/bin/sh` with the worktree as cwd. Keep them portable, idempotent, fail-fast,
and independent of absolute checkout paths. Reuse existing wrappers rather than copying their
implementation. Do not create empty or unconditional-success scripts for missing checks.

Run the applicable `bn build`, `bn build test`, or `bn build lint` entry points, then `bn verify`
for the complete selected gate. Avoid duplicate heavy runs when `bn verify` already exercises those
same scripts. Inspect the actual resolved script paths before execution so an unrelated global job
cannot masquerade as this repo's build. If the repo has no build/test/lint commands (for example a
docs-only repo), state that explicitly; do not invent a gate or claim verification passed.

### 6. Confirm and hand back

Rerun the selected `bn mcp init` command and inspect the diff: no duplicate managed blocks or server
entries, and unrelated content is unchanged. Confirm `bn status --json` and `bn --path` still point
at the intended note. Check catalog discovery with `bn skills find setup bn` and inspect the
selected harness's generated native skill file; discovery alone does not prove native installation.
Do not claim the running editor has loaded new files until it reloads its MCP servers/skills.

Record the commands and their source files in repo reference notes, including anything still
blocked. Close the setup todo with `bn done` only when its work is complete.

Report the repository/branch, note location, harness files changed, configured gate (or why none
exists), and any blocker. Distinguish repo-local readiness from optional machine dispatch readiness.
State whether machine settings were unchanged. If MCP was newly wired, tell the user to restart
the editor or reload MCP servers to pick it up.

Stop here unless machine configuration was requested or approved above.

## Machine configuration

Configure exactly one machine's repository access and resident runtimes in the synced bn
`config.toml`, then reconcile the services to match. When reached from repository setup, return to
the next repository step afterwards; do not treat machine registration alone as finished onboarding.

This is an operator-owned configuration change. Do not guess which repositories or background
services belong on a machine, do not grant `repos = ["*"]`, and do not modify another machine's
profile.

### 1. Identify the machine and configuration

Run:

```bash
bn setup
bn machine list --json
```

`bn setup` scaffolds the config when it is absent and reports the effective config path. Read the
complete file before editing it.

Resolve the exact, case-sensitive profile key using the same precedence as bn:

1. `$BN_HOSTNAME`, when set;
2. the top-level `hostname` key in the effective config;
3. the one-line `<config-dir>/machine` pin file.

Do not derive the key from a state-directory name. Show the resolved source and value to the user
before editing.

Use `bn machine list` only to identify possible old names whose history may need
`bn machine merge`. Cross-check a `current: true` row against the resolved key
case-insensitively, but never copy its spelling over the config/env/pin value. A brand-new machine
having no history row is normal and does not block setup.

### 2. Read the current profile

Find `[machines.<current-name>]` in the config. Report its current:

- `repos`
- `queue`
- `cron`
- `web`

A missing profile, repository, or flag is intentionally denied. Treat that as disabled, not as a
legacy run-everywhere default.

If the existing profile contains `repos = ["*"]`, surface that explicitly and replace it with the
concrete repositories the user approves. Do not silently preserve wildcard access.

Do not confuse these settings with:

- `[managed_repos]`, which controls clones bn refreshes;
- `[nodes].owned_repos`, which controls remote worker placement;
- `[jobs.only_on]` and `[[cron]].only_on`, which further restrict individual jobs.

### 3. Collect the desired registration

Use `ask_user` to collect all choices in one focused form:

1. The exact GitHub `owner/repo` slugs this machine may display, queue, and dispatch. Use full slugs
   whenever the repository has a remote; use a bare name only for a genuinely local repository.
   Show the current list as the default.
2. Whether the machine should run the queue dispatcher.
3. Whether the machine should register cron jobs.
4. Whether the machine should run bn-web.

Explain that selecting a runtime starts or registers a persistent background service, while
clearing it stops or prunes the bn-managed service. An empty repository list is valid and means
this machine cannot dispatch repository work.

If the user declines the form, make no changes.

### 4. Update only this machine

Edit or create exactly this table, preserving every unrelated key and every other machine:

```toml
[machines.<current-name>]
repos = ["owner/repo-a", "owner/repo-b"]
queue = true
cron = true
web = false
```

Normalize duplicates and surrounding whitespace. Do not silently shorten a full slug to its
basename: `owner-a/api` must not authorize `owner-b/api`. Adding another repository with the same
basename can make a bare-name grant ambiguous and fail closed, which is why full slugs are the
default.

`bn setup` may scaffold example profiles shipped by the repository. They are examples or another
machine's configuration, not permission to edit that machine. Preserve them unless the user
explicitly asks to remove them.

Before writing, show the current profile and replacement profile. This edit is reversible, but it
changes what background work can execute, so wait for explicit approval.

### 5. Validate the edit

Be explicit with the user: **every bn command loads the configuration and immediately reconciles
the managed queue and web services**. There is no read-only bn validation command for those two
runtimes. Cron changes are not applied until `bn cron register`.

After writing, run `bn queue status --json` and confirm:

- the queue state matches the chosen flag;
- no config parsing error was reported.

An enabled queue status payload does not include the machine name. When a disabled-runtime payload
does include one, confirm it matches the resolved profile key exactly.

If the identity or parsed policy is wrong, restore the last known-good profile immediately and run
`bn status` again. If web was enabled before the failed edit, also run `bn serve --daemon` to
restore it after rollback.

If queue is enabled, prove the repository boundary:

- for one configured repository, run a non-launching, harmless authorization probe such as
  `bn queue cancel __setup_probe_missing__ --repo owner/repo --json`; authorization should pass and
  return a success envelope with `"cancelled": false`;
- run the same probe against one locally known but unregistered repository and require the
  `repo_not_enabled` error;
- the internal queue repository `machine` remains reserved for repository-less machine jobs and is
  never added to the user's `repos` list.

If queue is disabled, both probes correctly stop at the queue-level `not_enabled` gate. Report that
the repository boundary cannot be probed through queue commands in this configuration; do not treat
that expected result as a failed edit or trigger rollback.

Do not use `bn dispatch` as a probe: it has no dry-run mode and may launch a real agent. Do not use
an unknown or uncloned repository for the rejection probe, because `unknown-repo` would not prove
the machine authorization boundary.

### 6. Reconcile runtimes

Once configuration parsing and repository authorization are valid:

1. Run `bn cron register --dry-run`.
2. Show the add/update/remove plan. If it removes any timer, require explicit confirmation before
   continuing. Confirm that a disabled cron result uses the expected machine name, so a typo cannot
   masquerade as an intentional prune.
3. Run `bn cron register` after approval. It installs allowed timers or prunes bn-managed timers on
   a disabled machine.
4. Run `bn status`. `bn queue ...` commands deliberately skip queue daemon installation, so this is
   the command that reconciles the enabled queue dispatcher. Web may already have reconciled during
   validation, and this confirms the settled state.
5. If web is enabled and not running, run `bn serve --daemon`.
6. If web is disabled, run `bn serve stop` unconditionally. It is idempotent and removes any
   managed, detached, or foreground listener that disabled status intentionally does not inspect.
7. Read `bn queue status`, `bn cron`, and `bn serve status`.

Expected disabled output is `not enabled on this machine`; disabled queue/web services must not be
running, and disabled cron must expose no registered bn job rows.

Never remove unrelated systemd, launchd, or Windows services. Use only bn's own lifecycle commands.

### 7. Report

Report one compact table:

| Machine | Repositories | Queue | Cron | Web |
|---------|--------------|-------|------|-----|

Then state the config path and whether reconciliation succeeded. If anything failed, name the exact
command and leave the profile in the last configuration that parsed successfully.
