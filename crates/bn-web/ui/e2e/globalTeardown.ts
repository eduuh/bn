// Playwright global teardown: stop the bn-web started in globalSetup.

import fs from "node:fs";
import { PID_FILE } from "./fixtures";

export default async function globalTeardown(): Promise<void> {
  try {
    const pid = Number(fs.readFileSync(PID_FILE, "utf8").trim());
    if (pid > 0) {
      try {
        process.kill(pid, "SIGTERM");
      } catch {
        /* already gone */
      }
    }
    fs.rmSync(PID_FILE, { force: true });
  } catch {
    /* no pid file — nothing to stop */
  }
}
