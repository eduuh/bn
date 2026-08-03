import type { Page, TestInfo } from "@playwright/test";
import { TOKEN } from "./fixtures";

/** Prime the paired web token into localStorage so writes are authorized (must run before goto). */
export async function primeToken(page: Page): Promise<void> {
  await page.addInitScript((t) => localStorage.setItem("bn-web-token", t), TOKEN);
}

/**
 * Pick a repo through the shared `RepoSelect` combobox.
 *
 * The repo fields used to be text inputs a spec could `.fill()`. They are now a button that opens a
 * cmdk list over `/api/repos`'s `known` set, so choosing a repo is: open, type to filter, click the
 * option. Centralized here because four specs drive it, and because a spec that still `.fill()`s
 * the id fails with a confusing "element is not an <input>" rather than "the picker changed".
 */
export async function pickRepo(page: Page, triggerId: string, repo: string): Promise<void> {
  await page.locator(`#${triggerId}`).click();
  const search = page.getByPlaceholder("Search repos…");
  await search.waitFor();
  await search.fill(repo);
  // Scope to the open dialog: the repo name may also appear in the page behind it.
  await page.getByRole("dialog").getByText(repo, { exact: true }).click();
  await search.waitFor({ state: "detached" });
}

/** Save a review screenshot for this viewport into the committed `e2e/screenshots/` folder. */
export async function shot(page: Page, testInfo: TestInfo, name: string): Promise<void> {
  // Let async queries + the SSE first frame settle so the shot is fully populated.
  await page.waitForTimeout(400);
  await page.screenshot({ path: `e2e/screenshots/${name}-${testInfo.project.name}.png` });
}
