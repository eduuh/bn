import { expect, test } from "@playwright/test";
import { seed } from "./fixtures";
import { primeToken, shot } from "./helpers";

test.beforeEach(() => seed());

test("skills list, view, and edit a skill — the edit persists on the server", async ({
  page,
}, testInfo) => {
  await primeToken(page);
  await page.goto("/skills");

  await expect(page.getByRole("heading", { name: "Skills", exact: true })).toBeVisible();

  // Both seeded skills list, tagged effective.
  const wrapSession = page.getByRole("button", { name: /wrap-session.*effective/ });
  await expect(wrapSession).toBeVisible();
  await expect(page.getByRole("button", { name: /plan-work.*effective/ })).toBeVisible();

  // Select one → its raw markdown loads in the editor.
  await wrapSession.click();
  const editor = page.getByRole("textbox", { name: "Edit wrap-session skill" });
  await expect(editor).toContainText("Wrap up a work session");

  await shot(page, testInfo, "skills");

  // Edit + save — a real PUT /api/skills/wrap-session {content}.
  const marker = "\n\n## Edited from the dashboard\n";
  const current = await editor.inputValue();
  await editor.fill(current + marker);
  const [req] = await Promise.all([
    page.waitForRequest(
      (r) => r.url().includes("/api/skills/wrap-session") && r.method() === "PUT",
    ),
    page.getByRole("button", { name: "Save skill" }).click(),
  ]);
  expect(JSON.parse(req.postData() ?? "{}").content).toContain("Edited from the dashboard");
  await expect(page.getByText(/Saved skill/)).toBeVisible();

  // Persisted: reload, reselect, and the edit is on disk.
  await page.reload();
  await page.getByRole("button", { name: /wrap-session.*effective/ }).click();
  await expect(page.getByRole("textbox", { name: "Edit wrap-session skill" })).toContainText(
    "Edited from the dashboard",
  );
});
