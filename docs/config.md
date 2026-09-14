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
| `web` | `true` | Run the bn-web dashboard. |

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
| `screenshot_dir` | none | Folder `bn shot` takes screenshots from. |
| `gif_dir` | none | Folder `bn shot --gif` takes recordings from. |

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
| `extras` | `[]` | Curated MCP servers `bn mcp init` wires beside bn's own. |

### `[managed_repos]`

| Key | Default | Meaning |
|---|---|---|
| `repos` | `[]` | Clone URLs bn keeps checked out under `~/.bn/repos`. |

### `[tools]`

| Key | Default | Meaning |
|---|---|---|
| `builtins` | `[]` | bn-shipped tool specs to enable on this machine, such as `n8n`. |

### `[links]`

| Key | Default | Meaning |
|---|---|---|
| `tags` | bn's built-in list | Tag vocabulary for `bn links`. `[]` means none. |

### `[web]`

| Key | Default | Meaning |
|---|---|---|
| `port` | `7770` | Port `bn serve` listens on. |
| `bind` | `"127.0.0.1"` | Bind address. `bn serve --lan` uses `0.0.0.0` for one run. |
| `fleet_poll_secs` | `2` | How often the dashboard resamples the fleet. |
| `tmux_mcp_autostart` | `true` | Start the dashboard from tmux hooks. |
| `tmux_mcp_badge` | `true` | Show the dashboard health badge in tmux. |

### `[log]`

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `false` | Write the structured action log. |
| `level` | `"debug"` | Lowest level written: `debug`, `info`, `warn` or `error`. |
| `dir` | `<state_dir>/logs` | Where log files go. |
| `retain_days` | `14` | Daily files kept. `0` keeps all. |

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

### `[jobs.only_on]`

| Key | Default | Meaning |
|---|---|---|
| `<job>` | runs everywhere | The machines a job may run on, such as `nightly-build = ["mac.lan"]`. |

### `[hosts.<alias>]` and `[nodes.<id>]`

`[hosts.<alias>]` adds what `~/.ssh/config` cannot say to `bn connect`. `[nodes.<id>]` declares
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

Job scripts under `config/jobs/` read their own `BN_*` parameters, documented at the top of each
script.
