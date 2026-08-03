# Gap backlog — dispatchable

Every item is sized to one agent and written so its **Dispatch** line can be pasted straight into
bn/web's Start form. Grouped into waves by *file overlap*, not by priority: two agents editing the
same file is the one failure mode that wastes the whole run, and `bn wt merge --all` serializes
overlapping PRs anyway.

## Wave 0 — prerequisite (not dispatchable)

**Land the live-test fixes first.** 21 uncommitted files on `playwright-testing-bn`, 4 commits
behind `origin/main`. Three later gaps (G1, G2, G7) build directly on the `ControlChannel` added in
`bn-core/src/agent/supervise.rs`; an agent branching from today's `origin/main` would not see it and
would either duplicate the work or conflict on the same 99 lines.

Also reinstall `bn-web` afterwards — `:7770` is still the pre-fix binary, so the dashboard you would
be watching the fleet from does not yet report `control`.

---

## Wave 1 — fully disjoint, safe to run in parallel

### G5 · ⌘K verbs
`ui/src/components/CommandPalette.tsx` only.

The palette reaches every screen but still only navigates. Add actions to the same ranked list:
`run <cron>`, `land <repo>`, `start <task>`, `warm <repo>`. Every endpoint exists; this is a dispatch
table over them. Route anything destructive through a confirm.

> **Dispatch:** Add verbs to the ⌘K palette in bn-web. It currently only navigates (destinations were
> added in the live-test pass). Add runnable actions to the same ranked list — run a cron, land a
> repo, warm a pool, start an agent — reusing the existing API clients in ui/src/api/writes.ts. Keep
> destinations ranked above verbs. Add unit tests to CommandPalette.test.tsx.

### G6 · Cost anomaly detection
`bn-core/src/cost.rs`, `bn-core/src/attention/`.

30 days of per-repo/model/day history and live per-agent `output_tokens`, with nothing watching it.
Compute a rolling median per repo and work-class; flag a *live* session at N× its cohort and raise
an attention item. `attention-sync` runs every 5 minutes and is the natural host.

> **Dispatch:** Add cost-anomaly detection to bn. The cost fold already computes per-repo/model/day
> rollups and the fleet carries live output_tokens per agent. Compute a rolling median per repo and
> work-class, flag any live agent burning N× its cohort, and raise it as an attention item from the
> attention-sync cron. Cost is the only bn signal that spends real money and currently has no
> reactor attached.

### G10 · Auth that matches `--lan`
`bn-web/src/routes.rs`, `bn-web/src/auth.rs`.

There is deliberately no token gate (#523) — the trust model is the bind address. Sound on loopback;
under `--lan` any device on the network can kill agents, merge PRs and rewrite skills. Keep loopback
open, require the already-persisted token only when bound off-loopback.

> **Dispatch:** In bn-web, require the paired web token for writes ONLY when bound off-loopback
> (--lan / an explicit non-127.0.0.1 --bind). Loopback stays ungated — that is the documented #523
> trust model and must not change. The token already exists at ~/.bn/config/web-token and the UI
> already sends it from localStorage. Reads stay open either way.

---

## Wave 2 — serialize (deep, shared files)

### G1 · Session-addressable agents
`bn-core/src/agent/supervise.rs`, `bn-cli/src/cmd/agent.rs`, `bn-web/src/actions.rs`.

Every write channel is keyed `repo:branch`: the tmux window name, the note's `agent-session:` record
(latest-wins), and all three endpoints. Two sessions on one branch are mutually unaddressable — the
live pass found **three** on `bn:perf-web-code-split`. Currently mitigated by refusing to act
(`ControlChannel::Ambiguous`); the capability is still missing.

> **Dispatch:** Make bn agent actions addressable by session id, not just repo:branch. Today the
> tmux window name, the note's agent-session record (latest-wins) and the send/relaunch/kill
> endpoints are all keyed repo:branch, so N agents on one branch cannot be told apart — supervise.rs
> now marks that ControlChannel::Ambiguous and refuses. Add a stable short session suffix to the tmux
> window name, record agent-session per session rather than latest-wins, add `bn agent send
> --session <id>` (and kill/relaunch), and have bn-web pass the session_id it already has. Retire the
> Ambiguous refusal once addressing works.

### G2 · "Why is it stuck?" — depends on G1
`bn-core/src/agent/supervise.rs`, `bn-core/src/agent/activity.rs`.

`stuck` is pure idle time, but the transcript's last entry almost always names the cause: a
permission prompt, a trust dialog, or a direct question. bn already reads those transcripts. Join
them, promote to a first-class health state, and surface the question with a reply box.

> **Dispatch:** Give bn a real reason for "stuck". Today AgentHealth::Stuck is pure idle time, but
> the agent transcript bn already parses usually ends with the cause — a permission prompt, a trust
> dialog, or a question to the human. Classify the trailing entry into waiting-on-question /
> waiting-on-approval / genuinely-idle, promote it to a first-class health state on the fleet
> payload, and surface the question text plus a reply box on the agent card in bn-web.

---

## Wave 3 — wide blast radius, run alone

### G4 · Retire `window.confirm`
13 call sites across `ui/src/routes/` and `ui/src/components/`.

Blocks the page, cannot be styled, cannot show a list or a diff, behaves badly in an iOS standalone
PWA, and silently turns a click into a no-op for any automation that does not handle the dialog.
Blocks G3.

> **Dispatch:** Replace every window.confirm in bn-web's UI (13 call sites) with one in-app
> ConfirmDialog component: title, arbitrary JSX body so a confirm can show a list or a diff, and a
> typed confirm phrase for the genuinely destructive ones (kill, land, reject). Keep every existing
> behaviour identical otherwise. Note the e2e suites accept native dialogs — update them.

---

## Wave 4 — after their dependencies

### G3 · Land pre-flight — depends on G4
`ui/src/components/LandButton.tsx` + a new panel.

"Land ready PRs" runs `bn wt merge --all`, documented as "the fast, **ungated** batch path" —
skipping the rebase-onto-main + verify gate a single `bn wt merge` applies by default, for every PR
at once, behind a confirm that names nothing.

> **Dispatch:** Give bn-web's "Land ready PRs" a pre-flight. It runs `bn wt merge --all`, which is
> the ungated batch path — it skips the rebase-onto-origin/main + verify gate a single `bn wt merge`
> applies by default. Show which PRs would merge, each with check status, whether it is rebased on
> current main, and its file-overlap group (merge --all already computes overlap). Default to the
> gated path; make ungated an explicit opt-in. Allow merging a selected subset.

### G7 · Controllability badge on fleet cards
`ui/src/lib/fleet.ts`, the fleet card component.

`control` is on the payload but only `SteerPanel` reads it. One glyph for drive-able vs
observed-only answers "why is that greyed out?" before the click.

### G9 · "While you were away" strip
New component + `ui/src/routes/Console.tsx`.

One line computed from the event log since your last visit: *"since 14:20 — 3 agents finished, 6 PRs
merged, 2 crons failed, 1 waiting on you"*, each segment a filter into the relevant screen.

### G8 · Duplicate-work detection
`bn wt merge --all` already computes changed-file overlap between PRs; the same computation over
*in-flight* worktrees would catch collisions while they are still cheap.

---

## Test-coverage gaps (fold into the above, not separate agents)

| Gap | Fold into |
| --- | --- |
| Steer/relaunch success path unproven — no addressable agent exists | **G1** (it creates one) |
| Plan review approve / request-changes never exercised | G4 (it touches both review panels) |
| Land's real merge path only ever run as a no-op | **G3** |
| Orchestrate daemon start/stop/scale, and queue-add's success path | own agent, later |
| Attention actions, goal note connect/disconnect, suggestions, `today/run` | own agent, later |
| Harness switching (copilot ↔ claude) | G4 (its confirm is one of the 13) |
| Multi-node — only one node exists | not testable here |
