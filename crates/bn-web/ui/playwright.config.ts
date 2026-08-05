import { defineConfig } from "@playwright/test";
import { BASE_URL } from "./e2e/fixtures";

/**
 * Playwright E2E for the bn-web dashboard (#479 / absorbs #477). `globalSetup` boots a real bn-web
 * against seeded fixtures; the specs drive the built UI headless and prove every flow (fleet render,
 * start an agent, steer/relaunch/kill, plan approve + request-changes, land, cost, token unlock).
 *
 * Two viewports run every spec — `mobile` (375px phone) and `desktop` — and the specs write review
 * screenshots for both into `e2e/screenshots/`, which are committed for the owner's design review.
 * Serial + single-worker because the specs share one server with mutable seeded state.
 */
export default defineConfig({
  testDir: "./e2e",
  testMatch: /.*\.spec\.ts/,
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: process.env.CI ? [["github"], ["list"]] : "list",
  globalSetup: "./e2e/globalSetup.ts",
  globalTeardown: "./e2e/globalTeardown.ts",
  timeout: 30_000,
  expect: { timeout: 10_000 },
  use: {
    baseURL: BASE_URL,
    trace: "on-first-retry",
    screenshot: "off",
  },
  projects: [
    {
      name: "mobile",
      use: {
        browserName: "chromium",
        viewport: { width: 375, height: 812 },
        isMobile: true,
        hasTouch: true,
        deviceScaleFactor: 2,
      },
    },
    {
      name: "desktop",
      use: {
        browserName: "chromium",
        viewport: { width: 1280, height: 900 },
      },
    },
  ],
});
