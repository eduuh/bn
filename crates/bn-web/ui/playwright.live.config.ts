import { defineConfig } from "@playwright/test";
import { BASE_URL } from "./e2e-live/live";

/**
 * Playwright config for the *live* sweep — it drives the bn-web the owner is already running
 * (`BN_LIVE_URL`, default `http://127.0.0.1:7770`) instead of booting one over fixtures.
 *
 * Run with: `npx playwright test -c playwright.live.config.ts`
 *
 * No `globalSetup`: the server is assumed up. Serial + single worker because the live fleet is
 * shared mutable state and two workers would race each other's SSE view of it.
 */
export default defineConfig({
  testDir: "./e2e-live",
  testMatch: /.*\.spec\.ts/,
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [["list"], ["json", { outputFile: "e2e-live/.report/results.json" }]],
  timeout: 60_000,
  expect: { timeout: 10_000 },
  use: {
    baseURL: BASE_URL,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  projects: [
    { name: "desktop", use: { browserName: "chromium", viewport: { width: 1440, height: 900 } } },
    {
      name: "mobile",
      use: {
        browserName: "chromium",
        viewport: { width: 390, height: 844 },
        isMobile: true,
        hasTouch: true,
        deviceScaleFactor: 2,
      },
    },
  ],
});
