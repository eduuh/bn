import type { Page, TestInfo } from "@playwright/test";
import { TOKEN } from "./fixtures";

/** Prime the paired web token into localStorage so writes are authorized (must run before goto). */
export async function primeToken(page: Page): Promise<void> {
  await page.addInitScript((t) => localStorage.setItem("bn-web-token", t), TOKEN);
}

/** Save a review screenshot for this viewport into the committed `e2e/screenshots/` folder. */
export async function shot(page: Page, testInfo: TestInfo, name: string): Promise<void> {
  // Let async queries + the SSE first frame settle so the shot is fully populated.
  await page.waitForTimeout(400);
  await page.screenshot({ path: `e2e/screenshots/${name}-${testInfo.project.name}.png` });
}
