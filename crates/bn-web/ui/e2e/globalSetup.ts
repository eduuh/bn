// Playwright global setup: seed the runtime tree, boot a real bn-web against it, wait until it is
// serving, and stash the pid for teardown. Everything the specs hit — reads, SSE, native writes,
// and process writes (via the stub `bn`) — is the real server over deterministic fixtures.

import { spawn } from "node:child_process";
import fs from "node:fs";
import {
  BASE_URL,
  HOST,
  LOG_FILE,
  PID_FILE,
  PORT,
  resolveBinary,
  seed,
  serverEnv,
} from "./fixtures";

async function waitForHealth(url: string, timeoutMs: number): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  let lastErr = "";
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`${url}/api/health`);
      if (res.ok) return;
      lastErr = `HTTP ${res.status}`;
    } catch (e) {
      lastErr = e instanceof Error ? e.message : String(e);
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  throw new Error(`bn-web did not become healthy at ${url} within ${timeoutMs}ms (last: ${lastErr})`);
}

export default async function globalSetup(): Promise<void> {
  seed();
  const bin = resolveBinary();
  const log = fs.openSync(LOG_FILE, "w");

  const child = spawn(bin, ["--port", String(PORT), "--bind", HOST], {
    env: serverEnv(),
    stdio: ["ignore", log, log],
  });
  child.unref();
  fs.writeFileSync(PID_FILE, String(child.pid));

  child.on("error", (e) => {
    throw new Error(`failed to spawn bn-web (${bin}): ${e.message}`);
  });

  await waitForHealth(BASE_URL, 20_000);
}
