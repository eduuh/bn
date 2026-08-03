// Shared plumbing for the *live* E2E sweep.
//
// Unlike `e2e/`, which boots a throwaway bn-web over seeded fixtures, this suite drives the real
// bn-web the owner is already running (default `http://127.0.0.1:7770`) against real state. The
// point is to catch what fixtures cannot: empty-state regressions, real-data shapes the components
// never saw, and console/network errors that only appear once the SSE stream is carrying live
// frames.
//
// Every spec here must stay non-destructive unless it is explicitly opted into via
// `BN_LIVE_WRITES=1`, because the fleet it observes contains the owner's actual running agents.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import type { Page, TestInfo } from "@playwright/test";

export const BASE_URL = process.env.BN_LIVE_URL ?? "http://127.0.0.1:7770";

/** The paired web token, read from the running host's config so writes are authorized. */
export function liveToken(): string {
  const explicit = process.env.BN_WEB_TOKEN;
  if (explicit) return explicit.trim();
  const p = path.join(os.homedir(), ".bn", "config", "web-token");
  if (!fs.existsSync(p)) throw new Error(`no web token at ${p}; set $BN_WEB_TOKEN`);
  return fs.readFileSync(p, "utf8").trim();
}

/** Prime the token into localStorage so authorized writes work (must run before goto). */
export async function primeToken(page: Page): Promise<void> {
  await page.addInitScript((t) => localStorage.setItem("bn-web-token", t), liveToken());
}

export type Diagnostic = {
  route: string;
  viewport: string;
  kind: "console" | "pageerror" | "requestfailed" | "httperror" | "envelope";
  text: string;
};

/** Diagnostics accumulated across the whole run; written out by the reporter spec. */
const REPORT_DIR = path.join(path.dirname(new URL(import.meta.url).pathname), ".report");

export function recordDiagnostics(rows: Diagnostic[]): void {
  if (!rows.length) return;
  fs.mkdirSync(REPORT_DIR, { recursive: true });
  const f = path.join(REPORT_DIR, "diagnostics.jsonl");
  fs.appendFileSync(f, rows.map((r) => JSON.stringify(r)).join("\n") + "\n");
}

/**
 * Attach listeners that collect everything the page complains about. Returns the live array —
 * read it after the navigation + settle window.
 *
 * Noise filter: react-query retries and the SSE reconnect both surface as transient
 * `requestfailed` on teardown, and Vite's HMR client logs on dev builds. Those are dropped so a
 * real error is never buried.
 */
export function watch(page: Page, route: string, viewport: string): Diagnostic[] {
  const out: Diagnostic[] = [];
  const push = (kind: Diagnostic["kind"], text: string) => {
    if (IGNORE.some((re) => re.test(text))) return;
    out.push({ route, viewport, kind, text });
  };

  page.on("console", (msg) => {
    if (msg.type() === "error" || msg.type() === "warning") {
      push("console", `[${msg.type()}] ${msg.text()}`);
    }
  });
  page.on("pageerror", (err) => push("pageerror", `${err.name}: ${err.message}`));
  page.on("requestfailed", (req) => {
    const f = req.failure()?.errorText ?? "unknown";
    // A navigated-away-from EventSource always aborts; that is not a defect.
    if (f.includes("net::ERR_ABORTED")) return;
    push("requestfailed", `${req.method()} ${req.url()} — ${f}`);
  });
  page.on("response", (res) => {
    if (res.status() >= 400) {
      push("httperror", `HTTP ${res.status()} ${res.request().method()} ${res.url()}`);
      return;
    }
    // A *process* write (dispatch, send, relaunch, land, …) replies `200` carrying `ok:false` —
    // the envelope, not the status line, is the source of truth. Watching only for 4xx/5xx would
    // score a failed steer as a clean run, so unwrap the envelope on every mutating 2xx.
    const method = res.request().method();
    if (method === "GET" || !res.url().includes("/api/")) return;
    void res
      .json()
      .then((body: { ok?: boolean; error?: { code?: string; message?: string } }) => {
        if (body && body.ok === false) {
          push(
            "envelope",
            `ok:false ${method} ${res.url()} — ${body.error?.code}: ${body.error?.message}`,
          );
        }
      })
      .catch(() => {
        /* not JSON — nothing to unwrap */
      });
  });

  return out;
}

const IGNORE: RegExp[] = [
  /\[vite\]/i,
  /React DevTools/i,
  /Download the React DevTools/i,
  // The service worker is a PROD-only progressive enhancement.
  /ServiceWorker|sw\.js/i,
];

/**
 * Navigate and let queries + the first SSE frame settle.
 *
 * `networkidle` is unusable here — `/api/stream` holds an `EventSource` open for the life of the
 * page, so the idle condition never fires and every visit would burn the full timeout. Waiting for
 * the shell's nav to paint and then a short settle is both faster and a stronger signal.
 */
export async function visit(page: Page, route: string): Promise<void> {
  await page.goto(route, { waitUntil: "domcontentloaded" });
  await page
    .getByRole("navigation", { name: "primary" })
    .waitFor({ state: "visible", timeout: 10_000 })
    .catch(() => {
      /* error routes have no shell; the caller asserts on that */
    });
  await page.waitForTimeout(700);
}

/** Save a review screenshot into `e2e-live/screenshots/`. */
export async function shot(page: Page, testInfo: TestInfo, name: string): Promise<void> {
  await page.screenshot({
    path: path.join(path.dirname(new URL(import.meta.url).pathname), "screenshots", `${name}-${testInfo.project.name}.png`),
    fullPage: true,
  });
}
