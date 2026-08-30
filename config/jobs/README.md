# Cron jobs

Version-controlled scripts that cron triggers via `bn run <job>`. The script is the durable
artifact — reviewable, diffable, in git — not an inline crontab command. This is the north
star in practice: AI authors a script here once; cron runs it forever, for free.

## Machine registration

The synced `config.toml` must explicitly register every machine and the repositories and resident
runtimes it may use:

```toml
[machines.MyLaptop]
repos = ["bn", "owner/arena"]
queue = true
cron = true
web = true
```

Use the exact identity reported by `bn machine`. A missing profile, repository, or flag is denied.
This is an intentional migration requirement: after upgrading, add a profile on every machine
before expecting cross-repo views, queue dispatch, cron jobs, or `bn-web` to run.

Machine profiles are the outer boundary. `[nodes].owned_repos` still limits worker placement,
`[jobs.only_on]` still limits individual job execution, and `[[cron]].only_on` still limits timer
registration. Disabling cron and rerunning `bn cron register` prunes bn-managed timers; disabled
queue/web services are unloaded by normal bn lifecycle reconciliation. Status commands remain
available and report `not_enabled`.

## How it works

- Each `*.sh` file is a job. `bn run meetings` executes `meetings.sh`. Many jobs are now native
  in-binary handlers instead (the `native_job` registry in `crates/bn-cli/src/cmd/job_registry.rs`);
  `bn run <job>` checks `native_job` first and only falls back to `<job>.sh` for un-ported jobs.
- `bn run` records every run (start, duration, exit code, output tail) to the event log
  (`<personal_root>/<machine>/state/events.jsonl`), so `bn cron status` and `bn doctor` can surface what
  ran and what failed.
- `bn cron` (alias `bn cron status`) is the unified view: every job joined with its next
  scheduled run and its last-run health. It shows which scheduler drives each job — a systemd
  `--user` timer or a Windows Task Scheduler task (the WSL-wake pattern below). The Windows side
  is queried over interop and cached (~1h) so the dashboard stays instant.
- `bn cron unregister <job>` stops and unregisters the systemd timer driving a job (the script
  stays). For a Windows-scheduled job it prints the `schtasks.exe … /DISABLE` to run instead.

## Two kinds of job: action and feed

A job either **does** work or **surfaces** it:

- **Action jobs** change something — `commit-notes` checkpoints notes, `refresh-all` pulls
  every repo, `daily-rollover` advances the day file, `recall-index` (re)builds the local
  semantic-recall index (`bn recall --index`; incremental + offline). The output is the side effect.
- **Feed jobs** answer "what should I look at?" — they fetch facts and drop them into the
  **feed**, an inbox you read with `bn feed`. A feed job is just an action job whose action is
  `bn feed ingest <source>`; there's no separate type to declare.

A feed job's script fetches (via `gh`, `az`, an MCP call, anything) and pipes one item per line
to `bn feed ingest <source>`:

    {"key":"pr-5377937","text":"Fix areaPath — Steph","url":"https://.../pullrequest/5377937"}

`key` is a stable id (for your eye only), `text` is the line shown, `url` is the link. Each run
**replaces** that source's items, so the feed mirrors the current state — merged PRs and past
meetings fall off on their own. `bn feed` renders every source grouped, newest write first;
`bn feed clear <source>` retires one.

    bn feed                    # render the inbox
    bn feed ingest <source>    # replace a source from JSON on stdin (the write path)
    bn feed clear  <source>    # forget a source

When a job should interrupt the `bn next` / attention list directly, pipe the same JSON/JSONL
shape into `bn attn ingest` instead of building a quoted command line:

    jq -c '.[] | {key:.id, title:.title, link:.url, severity:"warn"}' \
      | bn attn ingest --source <job> --replace

`--replace` makes the batch the source's current truth: keyed items no longer emitted by the job
are resolved automatically, while repeated keys are skipped instead of duplicated.

Crons are **headless**. `bn run <job>` executes the job in its own process and records the outcome;
no tmux session, window, or pane is ever created. A firing cron cannot steal your focus, clutter a
session, or leave a window parked behind it, because it never touches tmux at all.

Read a run's result from the event log rather than from a terminal:

- `bn cron` — every scheduled job with its last-run health (ok/FAIL, duration, run + failure counts).
- `bn queue badge` — the tmux status-bar signal for foreground queue attention, failures, or a
  stale dispatcher. Healthy routine cron/autonomous work stays quiet; inspect it with
  `bn queue timeline`.
- `bn run <job>` by hand — the job's output goes straight to your stdout.

The `class` field on a `[[cron]]` entry (`info` / `housekeeping`) is a descriptive dashboard label
only; it no longer changes how or where a job runs.

Because feed jobs almost always query private things (your PRs, your calendar, teammates), the
scripts live in a private notes `jobs/` dir, never here — see **Resolution** below. Two examples
of the shape (kept private on the machines that run them):

- **`ado-reviews`** / **`pr-queue`** — `az repos pr list --creator <alias>` (or `gh pr list`) for a
  set of followed authors → the review queue cache at `<state>/reviews.json`, so `bn reviews` and
  `bn next` are instant and offline. Each PR object is:

      {"repo","number","title","author","url","updated_at","created_at",
       "tag":"review"|"learning","reviewDecision":"APPROVED"|"CHANGES_REQUESTED"|"COMMENTED"|null}

  `reviewDecision` is **your** latest review state on the PR (from GitHub's `reviewDecision` /
  the viewer's latest review). bn uses it to drop PRs you've already approved or requested
  changes on from the "awaiting your review" action queue — stamp it, or leave it `null` and bn
  treats the PR as still awaiting you. Combined with `BN_GITHUB_LOGIN`, your own PRs and ones
  you've already actioned stay out of the queue; only PRs genuinely waiting on you become actions.
- **`meetings`** — today's meetings (from the workIQ / M365 Copilot CLI) → the feed, plus the
  single most-important one onto the **attention list** so it rides in `bn next`, plus a normalized
  **gap cache** (`<state>/meetings.json`) that `bn gap` and `bn next --fit` read offline (#247).
  workIQ judges which meetings are skippable; a local Copilot pass coerces its prose into JSON; jq
  fans it to the three surfaces. Shipped as `meetings.sh` here (it shells out to `workiq`, holds no
  private data — it's a clean no-op where `workiq` is absent). The feed + gap contracts are jq → bn:

      <query that returns today's meetings as JSON> \
        | jq -c '.[] | {key:.id, text:(.start + "  " + .subject), url:.joinUrl}' \
        | bn feed ingest meetings
      # and, for the gap cache `bn gap` / `bn next --fit` size against:
      <same JSON> | jq -c '.[] | {start, title:.subject, url:.joinUrl}' | bn gap ingest

  Scheduled weekdays at 14:50 by its agent manifest (config/agents/meetings.md, `catch_up: true` —
  the morning re-fire on the day's first bn event). When a meeting is upcoming it also
  writes a `meeting-prep` feed source — a few bullets (agenda, your role, the latest relevant
  thread, a doc to skim) — one extra workIQ call, cleared when nothing's upcoming.
- **`m365-sweep`** — one batched workIQ pass for everything that needs you, routed to bn's
  surfaces (the meetings pipeline, four signals at once so workIQ's latency is paid once):
  urgent **emails** → attention (reply-needed) + feed `email`; Teams **@-mentions** → attention +
  feed `teams`; today's **commitments** → daily goals; **owed replies** → feed `owe`. Each section
  is best-effort and timeout-bounded — a stall on one leaves that surface as-is and the rest run.
  **Conservative loudness:** an attention item is `--severity urgent` (and so toasts) ONLY when
  workIQ is confident it's high-stakes (manager/skip-level wrote, a deadline today, a direct
  question to you); everything else is `warn` — visible in `bn next`, silent. Like `meetings.sh`
  it ships here (shells out to `workiq`, no private data) and no-ops where `workiq` is absent.

### How the attention list groups it

`bn next` folds all of this into one ranked list, grouped into four sections so a mixed pile stays
scannable — ordered by what they demand of you:

    BREAKING         failing cron jobs, urgent flagged items
    WAITING ON YOU   PR reviews, urgent emails, Teams mentions (others blocked on you)
    TODAY            meetings, daily goals
    YOUR WORK        your stale PRs, blocked notes, the WIP nudge, misc flagged

(In the terminal each section carries a Nerd Font icon — bolt / eye / calendar / tasks — see
`crates/bn-core/src/glyph.rs`.)

The section is derived from the `--source` a feed/attn job stamps (`email`/`teams`/`meetings` get
their own glyph and land in the right section), so a new workIQ signal slots in just by choosing
its source. `bn next --json` carries the `category` per action for the prefix-J picker.

## Resolution

`bn run <job>` resolves `<job>.sh` across tiers, first match wins (like PATH):

1. **branch** — `<branch note dir>/jobs/`
2. **repo** — `<repo>/main note dir/jobs/`
3. **global · work notes** — `<work_root>/jobs/` (private, synced work notes)
4. **global · personal notes** — `<personal_root>/jobs/` (private, synced personal notes)
5. **global · shipped** — `$BN_JOBS_DIR`, default `~/.config/bn/jobs`, which `install.sh`
   symlinks onto *this* directory

So **sensitive jobs** (anything referencing your PRs, teammates, work, or private logic) live in
a private notes root's `jobs/` — never here. This directory holds only the **generic, shippable**
jobs (`ensure-tmux`, `refresh-all`, `daily-rollover`); a private job of the same name shadows the
shipped one. `<root>/jobs` sits at the notes-root level (sibling to `<machine>/`), so it's
machine-agnostic and syncs across your machines like the notes; use `[jobs.only_on]` for
per-machine differences.

## Markdown agents

`config/agents/*.md` is the declarative source of truth for agents. Every manifest must have YAML
frontmatter with an explicit `kind`:

| Kind | Contract |
|---|---|
| `command` | Runs a same-name native/script job or an explicit `run:` command. `gather:` is forbidden. |
| `staff` | Has a non-empty Markdown body, deterministic `gather:`, and at least one allowed `sinks:` entry. |
| `hosted` | Has a non-empty Markdown body consumed by its launcher. |
| `specialist` | Has a non-empty body plus `repo:` and compiles into a native reviewer; it cannot run or be scheduled. |

Runnable kinds may declare `on_calendar`, `retry_after` (default `15m` when scheduled),
`trigger: startup`, `network`, `queue`, and strict body `arguments`. Static role instructions and
output contracts belong in the Markdown body; Rust keeps deterministic gathering, validation,
security framing, routing, state, and structured-output parsing.

Agent resolution is first-match-wins by name:

1. branch note `agents/`
2. repo main-note `agents/`
3. personal overlay `agents/`
4. private work-notes `agents/`
5. private personal-notes `agents/`
6. shipped `config/agents/`

Overrides replace the whole manifest; they are not partial merges. To change a shipped prompt or
schedule, copy that agent into a higher-precedence tier with the same `name` and edit the copy.
For example, override `quarter-review` this way; the old `[quarter].review_cron` /
`BN_QUARTER_REVIEW_CRON` setting is rejected so two schedule authorities cannot coexist.

Use `bn agents validate` after editing a manifest, `bn agents` to inspect effective kinds and
schedules, and `bn agents render <name> --args '{"key":"value"}'` to diagnose strict templates.

## Scheduling

### Cadence ticks

Ten sub-hourly jobs used to own ten timers across three shared schedules. They now run behind
three **tick** crons — one timer each — that fan out to their cluster in order:

| Tick | Schedule | Runs |
|---|---|---|
| `tick-5m` | `*:0/5` | `tools-reconcile`, `routine-tick`, `attention-sync` |
| `tick-10m` | `*:0/10` | `auto-review-trigger`, `attention-auto-trigger`, `agent-supervise` |
| `tick-15m` | `*:0/15` | `auto-review-poll`, `cron-health` |

`agent-scheduler` runs every minute and owns every `on_calendar` declared in
`config/agents/*.md`; agent schedules do not belong in `crons.toml`.

Each child still runs through the normal `bn run` path, so it records its **own** `cron_run`
event. `bn cron` nests each schedule and its due/retry state beneath `agent-scheduler`; `bn doctor`
still sees each child's own failure history. A failing child never blocks its siblings, and the
scheduler records its own catalog/loop health separately.

Children have no `[[cron]]` entry of their own (that would schedule them twice) but remain
runnable by hand — `bn run notify` works exactly as before. A tick's `persistent` / `network`
flags are the OR of its children's.

Run a whole cluster on demand with `bn run tick-5m`.

### Keeping WSL awake

On WSL, systemd `--user` timers only fire while WSL is up, and WSL shuts down when idle. So bn
schedules with a two-part setup: **one** Windows task keeps WSL alive, and **systemd runs every
job** inside the now-always-up instance. Two installers recreate the whole thing on any machine.

### Recreate on a new machine

1. **Windows side** — register the single keepalive task (from PowerShell; pass your distro/user):

       pwsh -File config/jobs/windows/install.ps1 -Distro Ubuntu-22.04 -WslUser you

   It registers `BN WSL Keepalive` → `wsl.exe … bn run ensure-tmux`, triggered **at logon + every
   30 min**. `ensure-tmux` starts a detached tmux server, which boots WSL and keeps it alive
   afterwards. It also removes the older hand-made `BN Daily tmux` / `BN Commit Notes` tasks this
   design supersedes.

2. **WSL side** — install + enable all the job timers:

       sh config/jobs/systemd/install.sh

   This is a thin bootstrap: it finds the installed `bn` and runs **`bn cron register`**, the
   single source of truth on every platform. `bn cron register` strictly validates the Markdown
   agent catalog, then reads `config/crons.toml` (+ any per-repo `crons.toml`) and generates +
   `daemon-reload`s + `enable --now`s a `.service`/`.timer` into
   `~/.config/systemd/user/` for **every** declared cron — including the one common
   `agent-scheduler`, never one timer per agent — each
   `Persistent=true` so a run missed while WSL was down catches up on next boot, then prunes any
   `bn-*` timer no longer in the manifest. (`daily-rollover` fires ~05:00 — carry unfinished daily
   goals forward and drain due follow-ups. Attention toasts are owned by the `watch` cron
   (`notify` stays runnable by hand but is not on a tick). `agent-observe`
   fires every 30 min — fold finished Claude + Copilot sessions into the metrics log. Most runs
   are no-ops.) The top-level `install.sh` already runs `bn cron register` for you, so this script
   is only needed to bootstrap timers on a host where you didn't run the full installer.

`bn cron` then shows every job with its next fire time and last-run health — systemd timers and
the keepalive Windows task alike. It warns if the keepalive task is missing or disabled, since
that silently halts every downstream systemd job. Unregister a systemd timer with
`bn cron unregister <job>`.

### First-event catch-up

`Persistent=true` catches up a run missed while the machine was *off* — but some refreshes are
tied to *when you start work*, which varies day to day and rarely lands on a fixed timer slot.
For those, mark the manifest entry `catch_up = true`: on the **first bn event of the day** (any
bn command — the SessionStart `bn --cat`, a `bn next`, anything), bn fires `bn run <job>
--if-stale` for each such job, detached. It's gated to once per day by a sentinel in the state
dir, and `--if-stale` is a no-op when the job's timer already ran it today — so the job runs once
a day at the earlier of *your first activity* or its `on_calendar` time, never twice. Both
manifest kinds can opt in: `catch_up = true` on a `[[cron]]` entry, or `catch_up: true` in an
agent's frontmatter (meetings and m365-sweep use the latter).

`bn run <job> --if-stale` is the primitive on its own — call it from any hook to get
"refresh-on-first-touch" idempotency against the event log.

### macOS: launchd

macOS has no systemd. Use `launchd` — the installer ships in `launchd/`:

    config/jobs/launchd/install.sh        # bootstraps bn, then runs `bn cron register`

Like the systemd bootstrap, this is a thin wrapper that defers to **`bn cron register`** — the
single source of truth on every platform. It renders + (re)bootstraps a `com.bn.<job>` user-agent
plist into `~/Library/LaunchAgents` for **every** declared job (a `StartInterval` or
`StartCalendarInterval` translated from the manifest's `OnCalendar`; macOS runs a missed calendar
job when the machine wakes), then prunes any `com.bn.*` agent no longer in the manifest — so a
fresh macOS host gets all jobs, not a hand-picked pair. Same `bn run` underneath, so `bn cron` and
the event log work identically. (launchd can't express `RandomizedDelaySec` or a network-online
gate, so those manifest fields are dropped on macOS; every shipped schedule is verified
launchd-translatable by the `every_shipped_cron_translates_for_launchd` test.)

## Conventions

- Scripts must be idempotent — they may run repeatedly.
- Exit non-zero on failure so `bn cron status` / `bn doctor` flag it.
- Keep them POSIX-`sh` compatible (`bn run` invokes them with `/bin/sh`).

## Findings store (morning planning)

`bn findings` (#576) is an **append-only** history store — one durable run per source per day —
that feeds `today-agent` (#577) and the later planner slices (#898–#899). It is distinct from the
feed (which mirrors *current* state with replace semantics): findings **accumulate**, so a
run-over-run diff can tell "same item as yesterday" from a new one.

The **shipped** `gather-findings` cron (weekday 08:15, before `today-agent` at 08:30) snapshots the
already-local morning-planning caches into the store; the same work runs on demand with:

    bn findings gather            # snapshot every source (records only non-empty ones)
    bn findings gather --json     # machine-readable {source,tier,items,recorded,run_id}[]
    bn findings sources           # which sources have recorded runs
    bn findings query [--source s] [--since d] [--limit n]   # durable runs, newest first

Each adapter is a pure, **offline** read of a cache some other job already fills — additive (no
feed/attention/daily side effect), DRY (no duplicate fetch, no network), and Rust-only
(`crates/bn-core/src/findings_gather.rs`). An adapter that finds nothing records nothing (no empty
run rows), so off the work laptop the meetings/email/teams/owe passthroughs — and the
quarter/dojo/night-shift sources on a fresh box — are clean no-ops.

The stable **source ids** and their **item-key rules** (the id later slices depend on) are:

| source id | tier | reads (existing local cache) | item key |
|-----------|------|------------------------------|----------|
| `pr-review-queue` | inbound | reviews cache — PRs awaiting *your* review | `<repo>#<number>` |
| `my-prs` | in-flight | reviews cache — your own open PRs | `<repo>#<number>` |
| `meetings` | inbound | feed source `meetings` (passthrough) | upstream feed key |
| `email` | inbound | feed source `email` (passthrough) | upstream feed key |
| `teams` | inbound | feed source `teams` (passthrough) | upstream feed key |
| `owe` | inbound | feed source `owe` (passthrough) | upstream feed key |
| `night-shift` | in-flight | last night's PR-ready dispatches (`night::report_since`) | dispatch id |
| `cron-health` | health | alerting crons over cron status | cron job name |
| `sentinel` | health | attention items raised by `staff-sentinel` | attn key / title slug |
| `stale-branches` | in-flight | doctor note-findings (stale/no-goal/blockers) | `<repo>/<branch>` |
| `quarter` | alignment | current quarter's open goals (`quarter::read_quarter`) | goal key |
| `dojo` | growth | mined learning cards (`DojoStore::load_cards`, capped) | card id |

Tiers (from the morning-plan redesign) group sources for planning: **inbound** = needs your
response, **in-flight** = your work in progress, **health** = estate signals, **alignment** = goal
drift, **growth** = learning inputs. The item key is stable *within* its source only; the store row
is keyed by `(source, run_id)`.

## Feeding the metrics scorecard

`bn metrics` reads the **synced** productivity event log at
`<personal_root>/<machine>/metrics/events.jsonl` (durable across resets; the whole-person view
unions every machine's copy). Jobs append to it with one stable contract:

    bn event <kind> --field k=v --field k=v [--ts <epoch>]

All-digit values become JSON numbers (so `--field number=52` joins with PR metrics). `bn` itself
already emits `activity`/`note_closed` events on every mutation — those drive close-out lag, WIP,
and flow time with no job needed. The shipped **`pr-observe`** job supplies the rest: every 30
minutes it sweeps your own PRs on GitHub (`gh search prs --author=@me`, updated within 30 days)
and folds their lifecycle into the log — idempotent by `(kind, repo, number)` across every
machine's synced log, each event stamped at its GitHub timestamp so the first run backfills the
window truthfully:

| Event | Fields | Powers |
|-------|--------|--------|
| `pr_opened` | `repo`, `number` | throughput, in-flight, cycle time |
| `pr_merged` | `repo`, `number`, `lead_secs` | completion, cycle + **change lead time** (merge − first commit) |
| `pr_closed` | `repo`, `number` | completion rate |
| `pr_review` | `repo`, `number` | **time-to-first-review** + median review-latency metric (first non-author review) |
| `deploy` | `repo` | deployment frequency *(engine: planned; emit via `bn event`)* |

The weekly **shipped** `metrics-snapshot` job captures the scorecard so trends accrue over time.
The weekly **shipped** `weekly-insights` job renders `bn insights` (the auto-retro) into
`<notes>/<machine>/insights/<YYYY-Www>.md`, so each week's retrospective accumulates and syncs.

### Learning-dojo card mining (`dojo-cards`)

The nightly **opt-in** `dojo-cards` job (`[dojo].enabled`) mines new `bn dojo` learning cards for
the current repo so a session starts instantly from prepared material: bug mysteries from recent
`fix:` commits (symptom + pre-fix code shown, the fix hidden), walkthroughs from `feat:` commits,
and architecture tours from the module tree. It appends only cards with a new id (idempotent
re-mining) to the append-only store under `<notes>/<machine>/dojo/`, and honors the `[dojo] repos`
rotation allowlist. A completed `bn dojo` session emits a `dojo_session` metric event so streak +
habit drift can surface in `bn insights`.

| Event | Fields | Powers |
|-------|--------|--------|
| `dojo_session` | `cards`, `xp`, `streak` | dojo habit streak + learning-drift signal |

### Agent session telemetry (`agent-observe`)

The shipped `agent-observe` job folds finished Claude Code + GitHub Copilot CLI sessions into the
same metrics log as `agent_session` events — one per session — powering the `Agents` block of
`bn metrics` (sessions, agent-hours, model mix, Copilot AIU cost) and the per-branch recall view
`bn agent`. Unlike the `pr-*` jobs it writes via a **native** command, `bn agent ingest` (the
parsing spans two harness schemas and must dedup by session id), so the script is a one-line
wrapper. It's **per-machine** (each host reads only its own `~/.copilot` + `~/.claude`); the synced
log unions them, and the session-id dedup makes that safe with no `[jobs.only_on]` gating.

| Event | Fields | Powers |
|-------|--------|--------|
| `agent_session` | `session_id`, `harness`, `repo`, `branch`, `started_ts`, `duration_secs`, `turns`, `tool_calls`, `models`, `*_tokens`, `cost_nanoaiu` *(Copilot)*, `lines_*` | agent-hours, model mix, cost, per-branch recall |

### Live-agent supervision (`agent-supervise`)

Where `agent-observe` folds *finished* sessions into metrics, the shipped `agent-supervise` job
watches the **running** ones — the live-supervision layer the 24/7 orchestrator pattern needs.
The `tick-10m` cluster fires `agent-supervise` every 10 minutes: read each running Claude
session's last tool-activity timestamp, classify its health (active / idle / stuck against the
5-/15-minute defaults), and report the fleet. It's **read-only by default** (no surprise keystrokes
into a pane you're watching); set `BN_SUPERVISE_NUDGE=1` to also send a continue-nudge to each
stuck agent's tmux pane via `bn agent send`. The stuck detection + fleet shape live in the native
`bn agent supervise` (tested), so the script just decides whether to act. Per-machine like
`agent-observe` (reads only the host's own `~/.claude`); no `[jobs.only_on]` gating needed.

The same fleet surfaces in the cockpit **FLEET** panel and `bn agent supervise --json`. Two more
commands round out the loop: `bn agent checkin <mins> "<note>"` self-schedules a nudge via a
transient systemd `--user` timer (the `schedule_with_note.sh` analogue), and
`bn agent send <repo:branch> "<msg>"` delivers a message to a running agent's tmux pane
(send-keys — the `send-claude-message.sh` analogue).

Actionable transitions (`waiting`, `blocked`, `stuck`, `failed`, completion, and urgent attention)
are classified into the machine-local `wake-queue.json`; routine progress is absorbed. The queue
keeps per-consumer acknowledgements for `watch`, controller ids, and orchestrator repos, then
touches `wake-queue.signal`. Resident `bn watch`, `bn control watch`, and `bn orchestrate` processes
sleep on that signal instead of polling; their existing intervals remain fallback deadlines for
quiet agents becoming stuck or platforms where filesystem notifications are unavailable.
