# Configuration

bn needs no config file. Every setting below has a built-in default, and a machine with no config
at all works on every repository it has cloned and runs cron and the dashboard.

## Where settings live

| File | Holds |
|---|---|
| `<notes_root>/config.toml` | Settings every machine shares. It syncs with the rest of the notes tree. |
| `<notes_root>/<machine>/config.toml` | What this machine does differently. A key here overrides the shared file, key by key. |

`<notes_root>` is `~/projects/branch-notes`, or the work notes tree (`~/projects/notes/bn`) on a
machine that has only that one. `<machine>` is the name bn pinned in `~/.bn/config/machine` the
first time it ran.

bn reads one notes tree. The old `~/projects/branch-notes-work/bn` fallback is merged into
`~/projects/branch-notes` the first time a new `bn` runs: `tasks.txt` and `links.md` are unioned
line by line, other files are copied unless already present, and the old tree is kept as
`~/projects/branch-notes-work.migrated`.

Both files are optional. Leave out anything you do not want to change.

An old `~/.bn/config/config.toml` moves into these files the first time a new `bn` runs. The old
file is kept as `config.toml.bak`, and `~/.bn/config/config-migration.txt` lists what moved.

## What is not a setting

These are derived or fixed, so there is nothing to configure:

- **Which machine this is.** Pinned in `~/.bn/config/machine`.
- **Where the notes trees and worktrees are.** `~/projects/branch-notes`, `~/projects/notes/bn` and
  `~/projects/worktree`.
- **Whether a machine is work-only.** True when it has the work notes tree and no personal one.
- **Warm pool size.** Derived from the slots on disk.

## Settings

### `[machine]`

What a machine should do less of. Usually set in that machine's own file.

| Key | Default | Meaning |
|---|---|---|
| `repos` | `["*"]` | Repositories this machine may work on, as `owner/repo` or a unique repo name. |
| `cron` | `true` | Register and run scheduled jobs. |

### `[work]`

| Key | Default | Meaning |
|---|---|---|
| `work_repos` | `[]` | Repositories whose notes go to the work notes tree. |

### `[review]`

| Key | Default | Meaning |
|---|---|---|
| `weekly_anchor` | `"monday"` | Day the `bn weekly` window starts: a name, or `0` (Sunday) to `6`. |
| `sla_first_review_hours` | `4` | Hours before a PR waiting on your review counts as late in `bn next`. |
| `harness` | the running harness | Harness the auto-review agent uses: `claude` or `copilot`. |
| `diff_gate` | `false` | Require diff-only reviews before a dispatched ship PR lands. |
| `diff_gate_reviews` | `2` | Approving diff-only reviews the gate requires. |

### `[proof]`

| Key | Default | Meaning |
|---|---|---|
| `freshness` | `"exact"` | How current accepted evidence must be: `exact` (at HEAD), `ancestor`, or `any`. |
| `done_requires` | `"warn"` | What `bn done w<N>` does without accepted evidence: `off`, `warn`, or `refuse`. |
| `capture` | `true` | Keep `bn verify` and `bn verify --rebased` runs as proof, with their output attached. |

### `[harness]`

| Key | Default | Meaning |
|---|---|---|
| `default` | `"copilot"` | Harness used when a command does not name one. |
| `disabled` | `[]` | Harnesses turned off on this machine. `copilot` cannot be disabled. |

### `[attention]`

| Key | Default | Meaning |
|---|---|---|
| `max_age_hours` | `24` | Time-aged items older than this drop off `bn next`. Wider on Mondays. |

### `[calib]`

| Key | Default | Meaning |
|---|---|---|
| `s` | `"30m"` | Duration of an `--est S` item. |
| `m` | `"90m"` | Duration of an `--est M` item. |
| `l` | `"3h"` | Duration of an `--est L` item. |

### `[capture]`

| Key | Default | Meaning |
|---|---|---|
| `screenshot_dir` | none | Folder `bn note shot` takes screenshots from. |
| `gif_dir` | none | Folder `bn note shot --gif` takes recordings from. |

### `[wrap]`, `[standup]`, `[dxi]`

| Key | Default | Meaning |
|---|---|---|
| `wrap.enabled` | `false` | The end-of-day digest job. |
| `standup.enabled` | `false` | The standup autopilot job. |
| `standup.fetch_delta` | `false` | Also fetch the PR delta for standup. |
| `dxi.enabled` | `false` | The daily DXI self-rating prompt. |

### `[agents]`

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `[]` | Opt-in agents (manifests with `opt_in: true`) to switch on, by name. |

### `[notify]`

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | Desktop notifications at all. |
| `bar` | `"blocking"` | What may notify: `blocking`, `completion` or `all`. |
| `cooldown_mins` | `30` | How often a still-true condition may notify again. |
| `daily_budget` | `12` | Notifications per day. |
| `source_budget` | `4` | One source's share of the daily budget. |

### `[staff]`

| Key | Default | Meaning |
|---|---|---|
| `home` | none | The one machine the agent staff runs on. Unset, it runs anywhere. |

### `[mcp]`

| Key | Default | Meaning |
|---|---|---|
| `extras` | `[]` | Curated MCP servers `bn setup mcp init` wires beside bn's own. |

### `[mcp.servers.<name>]`

Your own MCP servers, registered at **user** scope on every machine. `[mcp].extras` is a closed,
vetted catalog written into *committable* per-repo configs; this is the opposite case — a personal
server that belongs to you rather than to any repo, and that should exist in every MCP client
wherever you sit down. `bn setup mcp sync-user` writes them into Claude Code, Claude Desktop, the VS Code
Copilot extension, and the Copilot CLI, whichever are present. `install.sh` runs it too, so a new
machine picks them up on install.

| Key | Default | Meaning |
|---|---|---|
| `command` | local servers | The executable to spawn. `~` and `$HOME` are expanded. |
| `url` | — | An HTTPS Streamable HTTP endpoint. Use this instead of `command`; supported for Claude Code and Copilot CLI. |
| `args` | `[]` | Arguments, each `~`/`$HOME`-expanded. |
| `env` | `{}` | Extra environment for the server process. Configuration only — never credentials. |
| `tools` | `["*"]` | The Copilot CLI tool allow-list. Other clients ignore it. |
| `clients` | local: all; remote: Claude Code and Copilot CLI | Restrict to supported client ids. Remote HTTP servers accept `claude-code` and `copilot-cli`. |

Paths are expanded at write time because clients spawn servers with no shell in between, so a
literal `~` would never resolve — and because expanding late is what lets one declaration serve a
Mac (`/Users/you`) and a Linux box (`/home/you`).

`~` expands only as a path *prefix* (a bare `~`, or a leading `~/`), exactly as a shell does; it is
a legal character elsewhere. Inside a composed argument — such as mcp-remote's `@<path>` — use
`$HOME`, which expands anywhere. Writing `@~/creds.json` is a **config error**, not a silent
mis-expansion: sync refuses it and names the fix.

Unlike `extras`, sync **overwrites** the entry under each declared name: this file is the source of
truth, and an edit here is meant to reach every machine. Servers bn does not declare are untouched.
Remote HTTP entries cannot use `args`, `env`, or `secret_file`, and custom HTTP headers are not
supported yet. The synced Context7 example therefore uses its unauthenticated endpoint.

#### `[mcp.servers.<name>.secret_file]`

A credential the server needs, which must not live in this file — `config.toml` is a synced git
repo. Declare only the *shape* of the secret; `bn setup mcp sync-user` prompts for the values once per
machine, writes them as a flat JSON object, and `chmod 600`s it. An existing file is never read or
rewritten, so re-running is a no-op. A server whose secret is missing is **skipped** rather than
registered, since a client would otherwise show it as permanently failing instead of absent.

| Key | Default | Meaning |
|---|---|---|
| `path` | required | Where the credential file lives. `~`/`$HOME`-expanded. Keep it out of any git repo. |
| `fields` | required | The JSON keys to prompt for, in order. |
| `hidden` | `[]` | Which of `fields` to read with terminal echo off. |

```toml
[mcp.servers.forge]
command = "npx"
args = [
    "-y", "mcp-remote@latest", "https://api.example.com/mcp",
    "--protocol", "auto",
    "--static-oauth-client-info", "@$HOME/.copilot/servers/forge-oauth-client.json",
]

[mcp.servers.forge.secret_file]
path = "~/.copilot/servers/forge-oauth-client.json"
fields = ["client_id", "client_secret"]
hidden = ["client_secret"]

[mcp.servers.context7]
url = "https://mcp.context7.com/mcp"
clients = ["claude-code", "copilot-cli"]
```

Context7's remote MCP endpoint can be registered without an API key; follow Context7's current
account guidance if you need higher rate limits.

### `[managed_repos]`

| Key | Default | Meaning |
|---|---|---|
| `repos` | `[]` | Clone URLs bn keeps checked out under `~/.bn/repos`. |

### `[links]`

| Key | Default | Meaning |
|---|---|---|
| `tags` | bn's built-in list | Tag vocabulary for `bn links`. `[]` means none. |

### `[forge_agent]`

Works Forge activities assigned to a family's agent member. The machine opts in by having an HTTP MCP server in `~/.claude.json` whose `Authorization` header carries an agent `fam_` key (Forge Settings → Family → Create agent key). The `forge-agent` job runs every five minutes from `tick-5m`. Each tick heartbeats or reports the runs this machine holds, then claims at most one task whose project repo URL matches a repo cloned here and runs `bn dispatch --agent` on it.

A claim is a lease, not a lock. While the agent runs, the tick renews it. If the machine dies, the lease expires and another machine can claim the task. When the agent runs `bn agent done`, bn kills its window and completes the activity, with the branch's PR in a comment. If the window disappears first, or the run passes `max_runtime_mins`, bn marks the activity Blocked instead. Use `bn forge agent status`, `run-once` and `release <id>` to inspect or steer it by hand.

| Key | Default | Meaning |
|---|---|---|
| `enabled` | automatic | `false` turns the loop off even when a Forge entry exists. |
| `server` | first `fam_` entry | The `mcpServers` entry name to use. |
| `max_concurrent` | `1` | Forge tasks this machine works at once. |
| `lease_secs` | `900` | Lease asked for on claim and each heartbeat. |
| `max_runtime_mins` | `120` | Kill a run older than this and report it blocked. |
| `harness` | dispatch's choice | `claude` or `copilot` for the dispatched agent. |
| `mcp_config` | `~/.claude.json` | JSON file with an `mcpServers` map to find the Forge entry in. Use a separate file so your own Claude sessions don't act as the agent. `BN_FORGE_AGENT_MCP_CONFIG` overrides it. |

### `[log]`

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `false` | Write the structured action log. |
| `level` | `"debug"` | Lowest level written: `debug`, `info`, `warn` or `error`. |
| `dir` | `<state_dir>/logs` | Where log files go. |
| `retain_days` | `14` | Daily files kept. `0` keeps all. |

### `[pool]`

| Key | Default | Meaning |
|---|---|---|
| `scheduled` | `[]` | Repos whose worktree pool the `pool-build` cron rebuilds at fixed times (02:00 and 07:30) instead of the idle-gated `pool-warm` sweep. Each run resets every free worker slot to one freshly fetched `origin/main` commit, builds it, and reports each slot's commit and outcome. |

### `[scout]`

| Key | Default | Meaning |
|---|---|---|
| `watch` | `[]` | Public `owner/repo` repositories the Scout watches for issues. |
| `tools` | `[]` | Public tool repositories whose changelogs it tracks. |
| `max_findings` | `5` | Most findings per run. |

### `[dispatch]`

| Key | Default | Meaning |
|---|---|---|
| `plan_review` | `false` | Make the plan-review gate the default for `bn dispatch`. |
| `plan_reviewers` | `3` | Independent plan reviewers. |
| `plan_review_quorum` | a majority | Approvals required. |

### `[pipeline.stages.<stage>]`

Also accepted as `[repo.<name>.pipeline.stages.<stage>]` for one repository. A field left out
inherits from the wider layer.

| Key | Default | Meaning |
|---|---|---|
| `model` | the harness default | Model the stage runs. |
| `effort` | the harness default | Reasoning effort. |
| `harness` | `--agent`, then `[harness].default` | Harness a dispatch at this stage launches. |
| `skill` | the stage's own skill | Workflow skill the stage runs. |
| `references` | inherited | Reference notes attached: `["*"]`, `[]`, or names. |
| `gate` | inherited | `auto` or `hold`. |

### `[repo.<name>]`

| Key | Default | Meaning |
|---|---|---|
| `verify` | bn's build, test and lint jobs | The repository's own verify command. |
| `requires` | `[]` | Node capabilities a target must have. |
| `excludes` | `[]` | Node capabilities that rule a target out. |

Use `bn verify --dry-run` to inspect the gate before running it. Resolution matches normal
verification: `--job <name>` selects a job script, otherwise `verify` selects the configured
shell command, otherwise all available build, test and lint jobs run in that order.
The preview shows the actual working directory and resolved command or script paths; it does
not execute checks, take a build slot, or record note progress or proof. A missing gate or
explicit job still fails. `--dry-run` cannot be combined with `--for`, and is not a `bn gate` flag.

`bn verify --dry-run --json` returns `data.dry_run: true`, `job`, `cwd`, and ordered `steps`.
Each step has `kind` and `label`, plus `command` for configured commands or `path` for scripts.
It deliberately omits `pass`, `code`, and `proof`: a successful preview is not a passing
verification run. `bn schema verify` documents both output forms.

### `[orchestrate]`

| Key | Default | Meaning |
|---|---|---|
| `keep_warm` | `false` | Keep resident supervisors supplied from open issues. |
| `own` | `[]` | Repositories keep-warm may feed. |
| `exclude_titles` | `["gap("]` | Issue titles keep-warm skips. Setting it replaces the list. |
| `exclude_labels` | `["blocked"]` | Issue labels keep-warm skips. Setting it replaces the list. |
| `low_water`, `brainstorm_count`, `brainstorm_interval_secs` | built in | Keep-warm pacing. |
| `stuck_idle_secs` | built in | Seconds idle before a working agent counts as stuck. |
| `min_free_ram_mb`, `max_loadavg`, `max_swap_pct`, `tokens_per_hour`, `daily_cost_usd`, `daily_cost_usd_coding`, `daily_cost_usd_research` | off | Pause local dispatch past these limits. |
| `remote_overflow` | `false` | Send `auto` work to a hosted runner when local is full. |

### `[tasks]`

| Key | Default | Meaning |
|---|---|---|
| `provider` | `"file"` | Where the task list lives. `"file"` is `<notes_root>/tasks.txt`. `"forge"` is the Forge app (forge.wira360.com), read and written through its MCP endpoint. It never falls back to the file. An unknown word is reported and uses `"file"`. |
| `forge_url` | `"https://api.wira360.com/mcp"` | The Forge MCP endpoint that `provider = "forge"` and `bn forge` talk to. |

With `provider = "forge"`, bn needs a Forge key for the family's agent member:

- `BN_FORGE_TOKEN`, when set, wins.
- Otherwise `<config_dir>/forge-token`, written with mode 0600 by `bn forge login`. The command
  opens Forge in a browser through `playwright-cli` (install `@playwright/cli`), waits for you to
  sign in, and mints the key inside the page. `bn forge login --paste` reads a key from stdin
  instead.
- Without a key, every task read and write fails and says to run `bn forge login`.

`bn forge status` shows where the key comes from (never the key), whether the MCP session opens,
the goals that carry a repository, and how many agent tasks are open and claimed.

In Forge mode an activity maps to a task like this: the title is the description, `src:forge:<id>`
links it back, `repo:` comes from the activity's meta or else the goal's repository, and
Urgent/High/Medium are `(A)`/`(B)`/`(C)`. A save sends only the fields that changed, and a meta
write that finds the activity changed since it was loaded fails instead of overwriting it. A new
task is created under the goal whose repository matches its `repo:`, or else under the goal named
Inbox. bn ids are never reused: the highest per endpoint is kept in `<state_dir>/forge-ids.json`.

`bn forge sweep` runs the agent loop once in Forge mode (the `forge-task` skill walks through it).
Nothing runs it automatically: run it by hand, or from your own cron.

- **Claim.** At most once a minute, an open task assigned to the agent member that nothing has
  started is claimed when its repository is cloned here, `[machine] repos` allows it, and this
  machine holds fewer than `--max` (default 1) claims without a PR. The claim writes
  `repo:<local clone>`, comments `claimed by bn@<node>`, and starts the task with
  `bn dispatch --task <id>`.
- **Comments.** Stage transitions, SMART refusals and `bn agent done` comment on the task. A failed
  comment is logged and never fails the step.
- **Merge.** `bn agent done --pr <url>` stores `pr:` and leaves the task open. The sweep asks
  `gh pr view` about it at most every ten minutes and completes the task once the PR is merged.

### `[jobs.only_on]`

| Key | Default | Meaning |
|---|---|---|
| `<job>` | runs everywhere | The machines a job may run on, such as `nightly-build = ["mac.lan"]`. |

### `[hosts.<alias>]` and `[nodes.<id>]`

`[hosts.<alias>]` adds what `~/.ssh/config` cannot say to `bn machine connect`. `[nodes.<id>]` declares
worker machines for placement, with `target`, `labels`, `ram_total_mb` and `repos`. Both are empty
by default.

## Environment

No setting is read from the environment. What bn does read:

| Variable | Purpose |
|---|---|
| `BN_HOME`, `BN_CONFIG_DIR`, `BN_CONFIG`, `BN_NOTES_DIR`, `BN_NOTES_DIR_WORK`, `BN_WORKTREE_DIR`, `BN_STATE_DIR`, `BN_JOBS_DIR`, `BN_AGENTS_DIR`, `BN_REFERENCES_DIR`, `BN_MANAGED_REPOS_DIR`, `BN_BIN_DIR`, `BN_MACHINE_FILE`, `BN_APPROVALS`, `BN_HOSTNAME`, `BN_OVERLAY_DIR`, `BN_PERSONAL_NOTES_DIR` | Path anchors. Test sandboxes use them to isolate a whole bn universe, and `BN_CONFIG` makes one file the entire config. |
| `BN_VERBOSE`, `BN_LOG` | Turn the action log on for one run. `BN_LOG` may also name a level. |
| `BN_COLOR` | `always` or `never` for terminal colour. `NO_COLOR` is honoured too. |
| `BN_AGENT_GUARD=0`, `BN_NO_CATCHUP` | One-run escape hatches: skip agent placement rules, skip cron catch-up. |
| `BN_DISPATCH_PIPELINE=1` | Make the legacy multi-stage pipeline the `bn dispatch` default. |
| `BN_FORGE_TOKEN` | The Forge key for `[tasks] provider = "forge"`. Wins over `<config_dir>/forge-token`. |

Job scripts under `config/jobs/` read their own `BN_*` parameters, documented at the top of each
script.
