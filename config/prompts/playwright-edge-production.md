---
name: playwright-edge-production
description: Drive any production or authenticated website in a visible Microsoft Edge automation profile over CDP from WSL
---

Use this skill together with the `playwright-cli` skill to automate a production
or authenticated website in the user's visible Microsoft Edge browser.

1. Determine the target URL from the user's request. If no URL was supplied,
   ask for one before launching the browser.
2. Run `~/.bn/config/repo/config/scripts/edge-cdp-wsl production '<target-url>'`.
3. If it reports that Windows blocked the relay, ask the user to approve the
   one-time firewall setup, run
   `~/.bn/config/repo/config/scripts/edge-cdp-wsl firewall`, and retry step 2.
   Never bypass or weaken the firewall globally.
4. The launcher attaches session `edge-production`, navigates directly to the
   target URL, and returns a snapshot.
5. Verify that the requested page loaded. If Edge shows a sign-in page, stop
   and ask the user to sign in manually in the visible browser. Never request,
   read, or enter credentials. After the user confirms, navigate to the target
   URL again and take another snapshot.
6. Perform the requested automation with
   `playwright-cli -s=edge-production ...`, using snapshot refs for elements.
7. When finished, run `playwright-cli -s=edge-production detach`. Do not use
   `close`, `close-all`, or `kill-all`; the persistent Edge profile must remain
   reusable and authenticated.

The launcher intentionally uses the dedicated Windows profile
`%LOCALAPPDATA%\Microsoft\Edge\PlaywrightProfiles\production`. Edge 136 and
newer reject CDP on the default user-data directory. The dedicated profile is
headed, persistent, and becomes authenticated after its first manual sign-in.
