# Live E2E — driving the bn-web you are actually running

`e2e/` boots a throwaway bn-web over seeded fixtures and asserts exact, known data. This suite is
the other half: it drives **the real bn-web on your machine**, against your real fleet, crons, cost
history and repos.

The two catch different things. Fixtures catch regressions in logic you already understand. This
catches what fixtures cannot — a component meeting a data shape nobody seeded, an empty state that
only appears when a list is genuinely empty, an action whose backing process is missing on this
host, and console/network errors that only appear once the SSE stream carries live frames.

## Running it

```bash
cd crates/bn-web/ui

# Read-only sweep + interactions + edge cases + safe writes, desktop and mobile.
npx playwright test -c playwright.live.config.ts

# One file, one viewport.
npx playwright test -c playwright.live.config.ts sweep.spec.ts --project=desktop
```

Point it elsewhere with `BN_LIVE_URL` (default `http://127.0.0.1:7770`). The server must already be
up — there is no `globalSetup`, on purpose: the whole point is to test the instance you use.

## The files

| Spec | What it proves |
| --- | --- |
| `sweep.spec.ts` | Every route renders against real data, throws nothing, and screenshots clean. |
| `interact.spec.ts` | Every control responds — palette, rail, tabs, filters, drill-downs, `j`/`k`, deep links, SSE. |
| `screens.spec.ts` | The affordances on screens the sweep only proved *render*: Insights' four views, the Research and Handoff forms, Notes selection, Attention actions, Goals meters, Nodes vitals. |
| `edge.spec.ts` | Unknown ids and unknown routes degrade gracefully instead of blanking. |
| `resilience.spec.ts` | Faults injected with `page.route`: a render throw, empty payloads, a dead server, a slow API, a 5xx. Never touches the real server. |
| `fixes.spec.ts` | Regression cover for every defect the first pass found, so none can come back quietly. |
| `writes.spec.ts` | Mutations that are inert, self-reverting, or already on a schedule. |
| `destructive.spec.ts` | Steer, relaunch, land. Opt-in only — see below. |

## Blast radius

`writes.spec.ts` is safe to run any time. Everything in it either round-trips back to the original
state, fires a job the scheduler already runs every few minutes, or asserts that a malformed
request is refused.

`destructive.spec.ts` drives your real agents and is skipped unless you ask for it:

```bash
BN_LIVE_DESTRUCTIVE=1 BN_SELF_TARGET="bn:my-branch" \
  npx playwright test -c playwright.live.config.ts destructive.spec.ts --project=desktop
```

`BN_SELF_TARGET` names the agent running the suite so it can never steer or relaunch itself. Steer
and relaunch additionally refuse to pick a healthy agent, and refuse any agent whose `control`
channel is `none` or `ambiguous` — they only act on a `stuck` agent bn can uniquely address, so the
feature is proven on the agent it exists to rescue and never on the wrong one.

**Known coverage gap.** That filter means the steer/relaunch *success* path only runs when such an
agent exists. On a host where every session was started outside bn (`control: none`) or where two
sessions share a branch (`ambiguous`), those two tests skip rather than fail. The skip reason names
which case you hit.

## Two things that will bite you when adding tests

**Native `window.confirm` gates most destructive buttons.** Playwright auto-dismisses dialogs, so
an unhandled confirm silently turns the click into a no-op — and a loose assertion will happily
call that a pass. Accept it explicitly:

```ts
page.once("dialog", (d) => void d.accept());
await button.click();
```

**A failed process write replies `200` with `ok:false`.** Status codes alone will not tell you a
steer failed. Assert on the envelope:

```ts
const [res] = await Promise.all([
  page.waitForResponse((r) => r.url().includes("/send") && r.request().method() === "POST"),
  send.click(),
]);
expect((await res.json()).ok).toBe(true);
```

`watch()` in `live.ts` unwraps every mutating 2xx for this reason, so `ok:false` shows up as an
`envelope` diagnostic rather than passing as a clean run.

## Output

- `e2e-live/screenshots/` — full-page shots per route per viewport, for design review.
- `e2e-live/.report/diagnostics.jsonl` — every console error, page exception, failed request and
  `ok:false` envelope seen during the run. An empty (absent) file means a clean sweep.
