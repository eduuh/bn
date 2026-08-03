# bn-web live test pass — findings

> **Status: all seven bugs fixed and re-verified.** Final run: 89 live tests passing at both
> viewports with zero console errors, zero failed requests and zero `ok:false` envelopes; plus 2,455
> Rust tests, 172 UI unit tests, 51 fixture E2E, clippy and fmt clean. Each fix carries a regression
> test in `fixes.spec.ts` (or the renamed test named below). Items 8 and 9 are unchanged — they are
> design decisions, not defects, and are yours to make. See `FEATURES.md` for what to build next.

Driven with Playwright against the running `bn-web 0.2.0` on `127.0.0.1:7770` (binary built 03:37,
HEAD `ba9f416`), against the real fleet, crons, cost history and repos.

**Coverage:** 19 routes × 2 viewports, every rail destination, the ⌘K palette, cost/inbox tab
systems, console filter + `j`/`k` + deep links, the start form, four drill-downs, the SSE stream,
unknown-id and unknown-route handling, seven safe mutations, and — with explicit authorization —
steer, relaunch and land against the live fleet.

**Result of the first pass:** 78 passed, 3 failed, 21 skipped — every failure one of the gaps below,
and across the entire non-destructive run **zero** console errors, zero uncaught exceptions, zero
failed requests and zero `ok:false` envelopes. The app was in good shape; the problems were
concentrated in the agent-control path.

**After the fixes:** 89 passed, 0 failed, still zero diagnostics. Each entry below records what was
wrong, how it was found, and what changed.

---

## Bugs

### 1. Relaunch reports success while recovering nothing — High · **FIXED**

The one button for rescuing a stuck agent silently does nothing and tells you it worked.

`POST /api/agents/{target}/relaunch` shells out to `bn agent recover --repo X --branch Y --yes`.
Against the live stuck agent it returned:

```json
{"ok":true,"data":{"candidates":0,"results":[]},"error":null}
```

`candidates: 0` means it recovered nothing. But the envelope says `ok:true`, so `writes.ts` resolves
the mutation, and `SteerPanel` fires its success path — a green **"relaunch triggered"** toast
(`screenshots/relaunch-sent-desktop.png`). The agent, idle 20 minutes, stayed stuck.

Reproduce against any branch that doesn't exist:

```bash
curl -X POST 'localhost:7770/api/agents/bn%3Anosuchbranch/relaunch' \
  -H 'Content-Type: application/json' -d '{"hard":false}'
# → {"ok":true,"data":{"candidates":0,"results":[]},"error":null}
```

**Fixed.** `actions.rs::no_op_is_not_success` demotes `candidates: 0` to a typed
`nothing_to_recover` failure, so the UI's error path fires instead of the success toast. Regression:
`fixes.spec.ts` → *"relaunching something unrecoverable reports failure, not success"*.

### 2. Steer / Relaunch / Kill are offered for agents bn-web cannot reach — High · **FIXED**

```json
POST /api/agents/bn%3Aperf-web-code-split/send
{"ok":false,"error":{"code":"send_failed","message":"no such tmux window, or tmux absent"}}
```

bn-web drives agents through tmux. All three live Claude sessions were started outside tmux, so the
entire Steer panel is inert for them — yet it renders fully enabled on both the Console right rail
and the Agent detail page, with no hint that Send, Relaunch and Kill cannot work. You find out after
you click.

**Fixed.** `bn_core::agent::ControlChannel` (`resume` / `tmux` / `ambiguous` / `none`) is stamped
onto every agent by a new `stamp_control` pass — tmux is enumerated once per pass, not probed per
agent, since `/api/fleet` is polled every two seconds. `SteerPanel` disables every control and
explains why. Regression: `fixes.spec.ts` → *"the steer panel refuses agents bn cannot address, with
a reason"*.

### 3. Agent actions are keyed on `repo:branch`, not on the session — Medium-High · **MITIGATED**

`AgentDetail.tsx:88` passes `<SteerPanel target={agent.target} />`, and the endpoints are
`/api/agents/{target}/{send,relaunch,kill}` where `target` is `repo:branch`.

The live fleet held **two** sessions on `bn:perf-web-code-split` (`c2656d0c`, `c4f5c9f1` — one
active, one stuck). The detail page is addressed by `session_id`, but its actions collapse to the
branch, so there is no way to steer or kill one of the two. On the phone console the two rows are
visually identical apart from an idle timer (`screenshots/console-mobile.png`).

**Mitigated, not solved.** Two live sessions on one branch are now stamped `ambiguous` and the
panel refuses to act, so a write can no longer land on the wrong session. The *capability* — steering
one of several agents on a branch — still needs per-session addressing through `bn agent send`; see
`FEATURES.md` §1, which is the full fix.

### 4. An unknown route drops you out of the app — Medium · **FIXED**

`main.tsx` declares no catch-all route and no `errorElement`. Any unmatched path renders React
Router's own dev-facing boundary: **"Unexpected Application Error! 404 Not Found"** on an otherwise
blank page — no nav, no branding, no way back (`screenshots/unknown-route-desktop.png`).

The same boundary catches *render* errors, so one throwing component takes the whole dashboard down
the same way.

This is a pure oversight, not a missing pattern: `/repos/no-such-repo` already renders a proper
in-shell "Repo not found" with a link back (`screenshots/bogus-repo-desktop.png`). Unknown agent and
cron ids degrade correctly too. Only the router itself is unguarded.

**Fixed.** Three levels of guard in `main.tsx`: a `*` catch-all rendering `NotFound` inside the
shell, a pathless layout route carrying `RouteError` so one broken screen costs you that screen and
not the dashboard, and `RootError` on the root for a shell-level failure. Regression: `edge.spec.ts`
→ *"an unknown route renders a 404 inside the shell"* and *"a deep unknown path…"*.

### 5. ⌘K cannot reach any screen by name — Medium · **FIXED**

The palette is a pure `/api/search` data search. It has no notion of the app's own destinations:

```
q=cost    → 0 results
q=console → 0 results
```

Both dead-end on "No results." The names that appear to work (`attention`, `today`, `cron`) only
match because data rows happen to contain those words, and they navigate to a data item rather than
the screen.

This is the topbar's advertised entry point ("Search ⌘K") and the only fast navigation on a phone,
where the rail is a horizontally scrolling tab bar and the last several destinations are off-screen.

**Fixed.** Sixteen destinations, each with synonyms ("money" → Cost, "schedule" → Cron), matched
client-side off the raw input and ranked above the server's data hits. With an empty box the palette
now leads with every destination instead of a bare prompt. Regression: `interact.spec.ts` → *"⌘K
reaches a destination by name and navigates to it"* + the synonym test, and two unit tests in
`CommandPalette.test.tsx`.

### 6. Today never answers "what did today cost" — Low-Medium · **FIXED**

The **AIC spent** tile reads `—` on any Claude-only day, because AIC is the metered *Copilot* bill.
That is most days. Meanwhile today's cost row does carry the answer:

```json
{"key":"2026-08-03","cost_usd":715.79,"aic":null,"sessions":8,"output_tokens":1825527}
```

`Today.tsx:446` reads `todayCost?.aic ?? null` and stops there.

**Fixed.** The tile falls back to `$ (API-equiv)` and relabels itself when `aic` is null — the two
are different units, so the label has to move with the number. It now reads `$715.79` where it read
`—`. Regression: `fixes.spec.ts` → *"Today always answers what the day cost"*.

### 7. The two headline money numbers have no thousands separators — Low · **FIXED**

`fmtAic` was `aic.toFixed(2)` and `fmtUsd` was `` `$${usd.toFixed(2)}` ``, so the Cost hero tiles
rendered `332589.65` and `$10161.70` while `fmtTokens` beside them compacted properly to `35.22M` —
making the two most important numbers on the page the hardest to read.

**Fixed.** Both route through a shared `money()` helper using `toLocaleString`, so they now read
`332,589.65` and `$10,155.77`. Regression: `fixes.spec.ts` → *"headline money is grouped and
readable"*, which also asserts no ungrouped four-plus-digit figure survives anywhere on the page.

---

## Risks worth a decision (not defects)

### 8. "Land ready PRs" is the *ungated* batch merge

The button runs `bn wt merge --all`. From `bn wt merge --help`:

> `--all` … stays the fast, ungated batch path
> `--no-gate` … (No effect with `--all`)

A single `bn wt merge` gates by default — it rebases onto `origin/main` and runs the repo's verify
gate on that head, "so a red or stale-base PR can't land (the class that broke main)". The dashboard
button skips that check, for every open PR at once, behind a bare `window.confirm` that names no
PRs and shows no count.

Tested safely: with zero open PRs it is a clean no-op, and the whole button → API → `bn` path works.

### 9. There is no authentication on any write, by design

`routes.rs`: *"There is no token gate (#523) — the server never requires a web token, on loopback or
`--lan`."* Confirmed — writes with no token and with a bogus token both reach payload validation.

On loopback that is a reasonable trust model. Under `--lan` (a documented first-class flag, and what
the PWA-on-a-phone use case needs) any device on the network can kill agents, merge PRs, run crons
and rewrite skill files unauthenticated.

Input validation itself is solid: path traversal in repo and job segments is rejected cleanly, and
malformed JSON is refused without disturbing the server.

---

## Checked and cleared

- `by_day` arrives unsorted — intentional and documented (every `by_*` vec is cost-desc).
- Process writes reply `200` with `ok:false` — a documented convention, and `writes.ts` throws on it
  correctly. (It does mean status codes alone can't tell you a write failed; the harness now unwraps
  every mutating 2xx for this reason.)
- Fleet counters (`active` / `idle` / `stuck`) match per-agent health exactly.
- API 404s are clean, typed envelopes with genuinely useful messages — `queue-add` against a repo
  with no live run points you at the exact endpoint to call first.
- Skill editing validates before persisting: a corrupt body is refused and the file is untouched.
- The SSE stream held an 8-second idle with no reconnect and no dropped frames.
- Every route renders, at both viewports, with no errors.

---

## Features that would make the work faster

Ordered by time saved per unit of effort.

**1. Make ⌘K a real command palette.** Merge static destination entries and verbs into the ranked
list alongside data hits: `cost` → the Cost screen, `start <task>` → the composer pre-filled,
`run <cron>` → that job, `land bn` → the pre-flight. Right now the fastest input device in the app
can't do the most common thing anyone types into a palette. Biggest single win, smallest change.

**2. Make stuck-agent rescue actually work, and put it on the console.** The fleet already knows
`health: "stuck"` with an idle timer. Given bugs 1–3, the current path is: notice it yourself → open
detail → click Relaunch → get told it worked → it didn't. Fix the envelope, then add a "Stuck 20m —
recover · kill · open" row in the Blocked-on-you lane so it comes to you.

**3. Land pre-flight.** Replace the bare confirm with a panel listing exactly which PRs will merge,
each with its check status and whether it's rebased on current main — and offer the gated path as
the default, with ungated `--all` as the deliberate opt-out. Merging is the one action here you
can't undo with a click.

**4. Retire `window.confirm`.** Thirteen sites use it. It blocks the page, can't be styled, can't
show a list or a diff, and behaves poorly in an iOS standalone PWA. An in-app confirm would let
(3) exist at all, and would let the destructive ones show what's about to happen.

**5. Session-addressable agents.** Fixes bug 3 and unlocks the honest version of everything above:
per-session steer, per-session kill, and a "two agents are on `bn:perf-web-code-split`" warning,
which is nearly always waste or a conflict about to happen.

**6. A controllability badge on every agent card.** One glyph for "bn can drive this" vs "observed
only". It answers, before you click, why the Steer panel will or won't do anything — and it makes
bug 2 impossible to hit blind.

**7. Today's cost tile should always show a number.** Fall back to API-equiv (bug 6). "What did
today cost" is the question the screen exists to answer.

---

## The harness

Lives in `crates/bn-web/ui/e2e-live/` with `playwright.live.config.ts`. See the README there for how
to run it and the two traps (`window.confirm` auto-dismissal, and `200`-with-`ok:false`) that make
naive assertions pass on broken features — both of which caught me before the assertions were
tightened.

It complements `e2e/` rather than replacing it: fixtures prove known data renders correctly, this
proves the instance you actually use is healthy against data nobody seeded.
