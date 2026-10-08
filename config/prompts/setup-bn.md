---
name: setup-bn
description: "Onboard a repo to bn: branch notes, agent integration, build and verify commands. Use when adding a new repo to bn or bn commands fail in a fresh clone."
when: Set up or onboard a new repo to bn, wire Copilot or Claude to its branch notes, configure its build/test/lint gate, or explicitly narrow what bn runs on one computer.
keywords: [setup bn, setup-bn, configure bn, new repo, new repository, repo setup, onboard repository, initialize bn, branch notes, mcp init, copilot, claude, build scripts, verification, machine config, narrow machine, disable cron]
---

You are the **bn repository setup** workflow. Make the target repository usable with bn: resolve
its branch notes, wire the agent's MCP integration, and establish accurate build and verification
commands. Work in the current repository unless the user names another path or remote.

Choose the scope from the request:

- **New repository (default):** follow Repository setup below. Do not run the machine workflow
  unconditionally.
- **Machine configuration only:** skip repository setup and follow Machine configuration below.
- **Both:** onboard the repository and use Machine configuration for the explicitly approved
  changes.

Preserve existing notes, user edits, MCP servers, instructions, and unrelated configuration.
Rerunning this skill must not duplicate entries, reset goals, or replace working scripts.
Do not commit, push, launch an agent, warm a pool, or install persistent services just to prove
setup works.

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

Reuse an existing checkout. If the user supplied only a remote and requested a clone, use
`bn wt clone <remote-url> [name]` and continue in the worktree it reports. Check for an existing
matching clone before doing so. Do not move or convert an existing clone into bn's bare/worktree
layout without approval. Outside Git, or on detached HEAD, report the missing repository/branch
context rather than creating a repository or switching branches silently.

### 2. Resolve note storage

bn needs no config file, and there is nothing to scaffold. Settings, when there are any, live in
the notes tree: `<notes_root>/config.toml` for every machine and `<notes_root>/<machine>/config.toml`
for this one, as described in the bn repository's `docs/config.md`. Native skills are installed by
`bn skills install`.

Establish whether the repository's notes belong in the personal or work store **before writing
notes**. Preserve an existing classification. For an unclassified repo, use an explicit user
instruction; otherwise ask which store to use, showing the resolved roots. Do not silently put work
material in personal notes. Work classification is `[work].work_repos` in the shared config, using
the normalized local repo name rather than the GitHub `owner/repo` slug. Preserve other entries.

A machine works on every repository it has cloned unless its `[machine]` table narrows `repos`. If
this machine's table does narrow it and the new repository is missing, propose adding **only this
repository** through Machine configuration below. If approval is declined, continue the repo-local
setup and report that the repository is excluded on this machine. Do not touch `[managed_repos]`,
worker placement, cron schedules, or runtime flags as a side effect of onboarding.

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
bn setup mcp init copilot
bn setup mcp init claude
```

Copilot writes `.github/mcp.json`, `.vscode/mcp.json`, and the managed block in
`.github/copilot-instructions.md`. Claude writes `.mcp.json` and the managed block in `CLAUDE.md`.
Use the command's JSON merge and instruction markers; do not replace entire existing files or
hand-write a machine-specific MCP socket/HTTP URL.

Do not pass `--with-extras` by default. Inspect `[mcp].extras` first: configured extras are included
even without the flag and may fetch packages on first launch. Surface that before wiring them and
obtain approval for additions beyond bn. Do not change the global extras list to work around a
repo-local choice.

The client starts `bn mcp serve` over stdio in its worktree. Report missing runners or a missing
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
- Use committed `.bn/jobs/<name>.sh` when shared project configuration is wanted. bn finds it in
  the branch's checkout (the conventional worktree path, else `git worktree list`), but confirm it
  resolves here: `bn build <name>` names the paths it searched when nothing resolves. Use the
  notes-repo location when it does not.
- If one existing native command is the correct complete gate, set
  `[repo.<resolved-name>].verify` to that command in the shared config, preserving other repo
  settings. This overrides the default build/test/lint sequence; it must not silently omit a
  required check.

Scripts run through `/bin/sh` with the worktree as cwd. Keep them portable, idempotent, fail-fast,
and independent of absolute checkout paths. Reuse existing wrappers rather than copying their
implementation. Do not create empty or unconditional-success scripts for missing checks.

Run the applicable `bn build`, `bn build test`, or `bn build lint` entry points, then `bn verify`
for the complete selected gate. Avoid duplicate heavy runs when `bn verify` already exercises those
same scripts. Inspect the actual resolved script paths before execution so an unrelated global job
cannot masquerade as this repo's build. If the repo has no build/test/lint commands (for example a
docs-only repo), state that explicitly; do not invent a gate or claim verification passed.

### 6. Confirm and hand back

Rerun the selected `bn setup mcp init` command and inspect the diff: no duplicate managed blocks or server
entries, and unrelated content is unchanged. Confirm `bn status --json` and `bn --path` still point
at the intended note. Check catalog discovery with `bn skills find setup bn` and inspect the
selected harness's generated native skill file; discovery alone does not prove native installation.
Do not claim the running editor has loaded new files until it reloads its MCP servers/skills.

Record the commands and their source files in repo reference notes, including anything still
blocked. Close the setup todo with `bn done` only when its work is complete.

Report the repository/branch, note location, harness files changed, configured gate (or why none
exists), and any blocker. State whether machine settings were unchanged. If MCP was newly wired,
tell the user to restart the editor or reload MCP servers to pick it up.

Stop here unless machine configuration was requested or approved above.

## Machine configuration

A machine works on every repository it has cloned and runs cron by default.
There is nothing to register. Change a machine only when the user asks it to do less, and change
only that machine's own file. When reached from repository setup, return to the next repository
step afterwards.

### 1. Identify the machine and its file

Run:

```bash
bn machine list --json
bn --path
```

The machine's name is the one-line `<config_dir>/machine` pin, or `$BN_HOSTNAME` when set. Its own
settings live in `<notes_root>/<machine>/config.toml`, the directory that holds the note subtree
`bn --path` shows; the shared settings are `<notes_root>/config.toml`. Show the resolved name and
both paths to the user before editing.

Use `bn machine list` only to spot old names whose history may need `bn machine merge`. A brand-new
machine having no history row is normal.

### 2. Read the current state

Read both files. Report the effective `[machine]` values, where a key in the machine file overrides
the shared file:

- `repos` (default `["*"]`, every cloned repository)
- `cron` (default `true`)

Do not confuse these settings with:

- `[managed_repos]`, which controls clones bn refreshes;
- `[nodes.<id>].repos`, which controls remote worker placement;
- `[jobs.only_on]` and `[[cron]].only_on`, which further restrict individual jobs.

### 3. Collect the change

Use `ask_user` (AskUserQuestion in Claude Code) to collect all choices in one focused form:

1. The repositories this machine may work on, as GitHub `owner/repo` slugs where the repository has
   a remote. Show the current list as the default.
2. Whether cron jobs should be switched off here.

Explain that with `cron = false`, `bn cron register` removes this machine's bn timers, and turning
it back on re-registers them. If the user declines the form, make no changes.

### 4. Update only this machine's file

Write only the keys that differ from the defaults, preserving everything else in
`<notes_root>/<machine>/config.toml`:

```toml
[machine]
repos = ["owner/repo-a", "owner/repo-b"]
cron = false
```

Normalize duplicates and surrounding whitespace. Do not silently shorten a full slug to its
basename: `owner-a/api` must not authorize `owner-b/api`, and a bare name two owners share is
ambiguous and fails closed. Removing a key restores its default.

Before writing, show the current and replacement tables. This edit is reversible, but it changes
what background work can execute, so wait for explicit approval.

### 5. Validate and reconcile

Every bn command loads the configuration. Cron changes are applied by `bn cron register`.

1. Run `bn status` and confirm there is no config parsing error.
2. Run `bn cron register --dry-run` and show the plan. If `repos` was narrowed, confirm that crons
   declaring a repository outside the list are not in it. Do not use `bn dispatch` as a probe. If
   the plan removes any timer, require explicit confirmation, then run `bn cron register`.
3. Read `bn cron`.

If anything is wrong, restore the previous table and run `bn status` again. Never remove unrelated
systemd, launchd, or Windows services; use only bn's own lifecycle commands.

### 6. Report

Report one compact table:

| Machine | Repositories | Cron |
|---------|--------------|------|

Then state the file changed and whether reconciliation succeeded. If anything failed, name the
exact command and leave the file in the last configuration that parsed successfully.
