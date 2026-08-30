---
name: setup-bn
description: "Register this machine's bn repositories and opt-in queue, cron, and web runtimes"
when: Set up bn on a new computer, change which repositories this computer may run, or enable or disable its queue, cron, or web services.
keywords: [setup bn, configure bn, register machine, register repos, machine profile, enable queue, enable cron, enable web]
---

You are the **bn machine setup** workflow. Configure exactly one machine's repository access and
resident runtimes in the synced bn `config.toml`, then reconcile the services to match.

This is an operator-owned configuration change. Do not guess which repositories or background
services belong on a machine, do not grant `repos = ["*"]`, and do not modify another machine's
profile.

## 1. Identify the machine and configuration

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

## 2. Read the current profile

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

## 3. Collect the desired registration

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

## 4. Update only this machine

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

## 5. Validate the edit

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

## 6. Reconcile runtimes

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

## 7. Report

Report one compact table:

| Machine | Repositories | Queue | Cron | Web |
|---------|--------------|-------|------|-----|

Then state the config path and whether reconciliation succeeded. If anything failed, name the exact
command and leave the profile in the last configuration that parsed successfully.
