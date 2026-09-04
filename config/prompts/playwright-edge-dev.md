---
name: playwright-edge-dev
description: "Drive localhost:3000/chat in a visible authenticated Microsoft Edge automation profile over CDP from WSL"
---

Use this skill together with the `playwright-cli` skill to automate the local
chat application in the user's visible Microsoft Edge browser.

## Prerequisite: `playwright-cli`

Install it before anything else — the launcher exits 1 without it:

```sh
npm install -g @playwright/cli
```

**The package is `@playwright/cli`, NOT the npm package literally named
`playwright-cli`.** That one is the deprecated pre-1.0 Playwright CLI
("Deprecated package, use @playwright/cli instead.") and installing it yields a
command of the same name with a completely different interface.
`@playwright/cli` is what ships `bin: { 'playwright-cli': ... }` and the
`-s=<session>` / `attach` / `detach` / `tab-list` / `tab-select` / `snapshot` /
`find` / `run-code` / `--raw eval` commands used below.

Edd's dotfiles install it via `install_playwright_cli` in
`.bin/setup/common.sh`.

1. Confirm the application is already available at
   `https://localhost:3000/chat`. Do not start or replace the user's dev server
   unless explicitly requested.
2. Run `~/.bn/config/repo/config/scripts/edge-cdp-wsl dev`.
3. If it reports that Windows blocked the relay, ask the user to approve the
   one-time firewall setup, run
   `~/.bn/config/repo/config/scripts/edge-cdp-wsl firewall`, and retry step 2.
   Never bypass or weaken the firewall globally.
4. The launcher attaches session `edge-dev`, navigates to
   `https://localhost:3000/chat`, and returns a snapshot. It inspects the SPA
   Landing Redirect marker **only when the browser comes to rest on a
   `signinDev` page**, because the extension injects its content script on
   `https://localhost/signinDev*` and nowhere else. Landing on `/chat` at all is
   itself proof the redirect worked. Treat the marker as diagnostic only; it is
   never a prerequisite for using an already authenticated app.
5. After the launcher returns, use `playwright-cli -s=edge-dev tab-list`, select
   the tab whose URL starts with `https://localhost:3000/chat`, and take a
   snapshot. If `textbox "Message Copilot"` is visible, the app is authenticated
   and ready. Continue immediately even if the launcher reported a missing
   marker or opened `edge://extensions`; do not inspect, reload, or ask about the
   extension.
6. Only when the composer is absent, inspect the selected chat tab. If Edge
   shows a sign-in page, stop and ask the user to sign in manually in the
   visible browser. Never request, read, or enter credentials. If Edge shows a
   local certificate interstitial, ask the user before bypassing it. Troubleshoot
   the SPA Landing Redirect extension only when the unusable chat tab shows that
   its redirect is required.
7. Before any timer- or rAF-driven app work (blank/empty response detection is
   rAF-driven), foreground the tab and CONFIRM
   `--raw eval "document.visibilityState"` returns `"visible"`. A hidden tab
   pauses `requestAnimationFrame`, so that code stalls silently with zero errors
   and looks like nothing happened. See the Discoveries note on minimized
   windows if it stays `"hidden"`.
8. Perform the requested automation with
   `playwright-cli -s=edge-dev ...`, using snapshot refs for elements.
9. When finished, run `playwright-cli -s=edge-dev detach`. Do not use `close`,
   `close-all`, or `kill-all`; the persistent Edge profile must remain reusable
   and authenticated.

The launcher intentionally uses the dedicated Windows profile
`%LOCALAPPDATA%\Microsoft\Edge\PlaywrightProfiles\dev`. Edge 136 and newer
reject CDP on the default user-data directory. The dedicated profile is headed,
persistent, and becomes authenticated after its first manual sign-in.

Never launch this profile with `--disable-extensions`,
`--disable-extensions-except`, or another blanket extension-disable flag.
Managed/device-auth extensions and the SPA Landing Redirect extension must
remain enabled for local sign-in. Do not use `--load-extension`; managed Edge
may ignore it, so load the unpacked extension manually once and reuse the
profile.

## Discoveries - making the flow seamless

- **First run on a new profile is never unattended.** The dedicated
  `PlaywrightProfiles\dev` profile starts empty, so it needs TWO one-time manual
  steps on the Windows desktop: load the unpacked extension, and complete an
  interactive Microsoft sign-in. Do not try to automate either. Never request,
  read, or type credentials. Once done, the profile is persistent and both stay.
- **`bringToFront()` cannot raise a MINIMIZED window.** If the Edge window itself
  is minimized or fully occluded, switching tabs does not make the page visible.
  The signature is `document.hasFocus() === true` while
  `document.visibilityState === "hidden"` - focus without visibility. Retrying
  `bringToFront()` never fixes it; ask the user to restore the Edge window on the
  Windows desktop. Check both at once:
  `--raw eval "JSON.stringify({vis: document.visibilityState, focus: document.hasFocus()})"`
- **`--raw eval` prints the JSON-ENCODED value.** `document.visibilityState`
  comes back as `"hidden"` *with quotes*, not bare `hidden`. Any shell comparison
  must strip them (`${val//\"/}`) or match loosely - comparing `"$v" == "1"`
  against a returned `"1"` silently never matches. This bug made
  `edge-cdp-wsl`'s extension gate unpassable.
- **No Edge profile available?** The repo's own mock harness needs no browser
  profile and no auth: from `packages/web-app`,
  `yarn playwright test -c playwright.config.streamhub.ts src/e2e/streamHub/`
  (`mockuser=1`, `mockdata=1`). It cannot exercise real WebApp, but it drives the
  full FluxV4 StreamHub task lifecycle deterministically.

- **Foreground the tab before any timer/rAF-driven app work.** A backgrounded
  tab reports `document.visibilityState === "hidden"`, which pauses
  `requestAnimationFrame`. App code that schedules through rAF (e.g. blank/empty
  response detection) then stalls and never fires - with zero errors, so it
  looks like nothing happened. Raise the tab first:
  `playwright-cli -s=edge-dev run-code "async (page) => { await page.bringToFront(); }"`
  then verify `--raw eval "document.visibilityState"` returns `"visible"`.
- **`run-code` takes a function, not a statement.** Pass
  `async (page) => { ... }`; a bare top-level `await page.bringToFront()` throws
  `Unexpected identifier 'page'`.
- **`run-code` echoes the snippet, not its return value.** To read in-page
  state use `--raw eval "<expr>"` (it prints the returned value); wrap objects in
  `JSON.stringify(...)`.
- **The extension marker gate can be a false negative.** The launcher may report
  the SPA Landing Redirect marker missing even when `https://localhost:3000/chat`
  already loads authenticated (title "Microsoft Copilot", composer
  `textbox "Message Copilot"` present). Select the chat tab with
  `playwright-cli -s=edge-dev tab-list` and
  `playwright-cli -s=edge-dev tab-select <index>`, then prefer the functional
  composer check over the gate. Continue immediately when the composer is
  visible; do not inspect or ask about the extension.
- **A full `goto` reload clears injected page globals.** Anything captured on
  `window` (e.g. a webpack/rspack `require` handle) must be re-captured after
  navigation; SPA in-page navigation preserves it.
- **Reaching app internals:** capture the rspack require via
  `window.rspackChunk_bigmono_m365_copilot_app.push([[k],{},r=>window.__req=r])`;
  module factories live in `window.__req.m`. Lazy modules load only after the
  owning surface renders (open the relevant conversation/route first).
- **Client telemetry path for verification:** the app batches events to
  `POST https://localhost:3000/events` (its own proxy) -> server OneDS/Pacman ->
  Kusto, *not* the browser's direct OneCollector. Confirm emits by grepping the
  dev server log for `[CopilotEventLogger] Event:`,
  `[OneDsDispatcher] ... core.track`, and `[PacmanService] Events logged
  successfully`. Local dev (`NO_AUTH`, dogfood SDF ring) ingests into the
  **WebApp Dev** Kusto db, not the Dogfood-prod db.

## Best flow - validate app telemetry/detection end-to-end

Use this exact order whenever you must prove the app's OWN code emits an event
in a real browser (do not mock the emit path):

1. **Attach + authenticate** (steps 1-8 above). Prefer a functional check
   (composer visible) over trusting the extension marker gate.
2. **Confirm the branch code is in the running bundle.** Capture the rspack
   require (`window.rspackChunk_bigmono_m365_copilot_app.push([[k],{},r=>window.__req=r])`)
   and grep `window.__req.m` factory sources for the new symbol/event id. If the
   owning module is lazy, open the surface (conversation/route) that loads it
   first.
3. **Foreground the tab** and verify `--raw eval "document.visibilityState"` is
   `"visible"` - otherwise any `requestAnimationFrame`/timer-driven code stalls
   silently.
4. **Force the target state deterministically against the real DOM.** Beware a
   virtualizer: a *missing* node may read as scrolled-away (visible). Inject the
   real element the detector inspects rather than deleting one.
5. **Run the app's own entry point** (not a reimplementation) against that DOM,
   and **wrap the real logger** to capture calls - read it back with
   `--raw eval "JSON.stringify(window.__calls)"`.
6. **Confirm forwarding + ingestion** by grepping the dev server log for the
   client->OneDS->Pacman chain (`[CopilotEventLogger] Event:` ->
   `[OneDsDispatcher] ... core.track` -> `[PacmanService] Events logged
   successfully`), then query the **WebApp Dev** Kusto db by
   conversationId + eventId (a passed clientCorrelationId may be replaced by the
   session's).
7. **Capture proof** (screenshot + a short markdown note) and `detach`.
