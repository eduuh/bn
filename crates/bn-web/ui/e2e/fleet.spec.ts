import { expect, test } from "@playwright/test";
import { seed } from "./fixtures";
import { primeToken, shot } from "./helpers";

test.beforeEach(() => seed());

test("fleet renders live agents, needs-you, and the bottom tab-bar", async ({ page }, testInfo) => {
  await primeToken(page);
  await page.goto("/fleet");

  await expect(page.getByRole("heading", { name: "Fleet", exact: true })).toBeVisible();

  // "Orchestrators" pins the live wave-runner (#500) in a distinct top tier, with its role badge.
  await expect(page.getByRole("heading", { name: "Orchestrators", exact: true })).toBeVisible();
  const orchestrators = page.getByRole("region", { name: "Orchestrators" });
  await expect(orchestrators.getByText("◆ Orchestrator", { exact: true })).toBeVisible();

  // "Needs you" pins the plan-waiting (501) and errored (505) agents at the top.
  await expect(page.getByText("Needs you")).toBeVisible();
  await expect(page.getByText("dispatch/plan-review-501")).toBeVisible();
  await expect(page.getByText("dispatch/dns-505")).toBeVisible();

  // A plain working agent shows in the Working section.
  await expect(page.getByText("dispatch/web-redesign-500")).toBeVisible();

  // Two surfaces, one board: a card is spent only where a decision is waiting, so the working tier
  // is rows. A needs-you card carries its repairs; a working row carries none.
  const needs = page.getByRole("region", { name: "Needs you" });
  const working = page.getByRole("region", { name: "Working" });
  await expect(needs.getByRole("button", { name: /Relaunch/ }).first()).toBeVisible();
  await expect(working.getByRole("button", { name: /Relaunch/ })).toHaveCount(0);

  // The harness is a chip that discloses a menu — never a permanent segmented block on every card.
  await expect(page.getByRole("menu", { name: "Switch harness" })).toHaveCount(0);
  const harnessChip = needs.getByRole("button", { name: /claude|copilot/ }).first();
  await harnessChip.click();
  await expect(page.getByRole("menu", { name: "Switch harness" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu", { name: "Switch harness" })).toHaveCount(0);

  // One primary nav in both layouts now — the rail re-flows into the phone tab bar rather than a
  // second, separate nav existing for small screens (clay redesign, #966).
  const nav = page.getByRole("navigation", { name: "primary" });
  await expect(nav).toBeVisible();
  // The pinned verb is "New task", not "Start an agent": work only ever starts from a task (#1006),
  // so the rail stopped carrying a Start link. Starting an agent is the page's own header button.
  await expect(nav.getByRole("link", { name: /New task/ }).first()).toBeVisible();
  await expect(nav.getByRole("link", { name: "Fleet" }).first()).toBeVisible();

  await shot(page, testInfo, "fleet");
});

test("fleet: an agent shows the task it is working on, and the chip jumps to it", async ({
  page,
}) => {
  await primeToken(page);
  await page.goto("/fleet");

  // The board could always say an agent was *running*; it could not say what it was running.
  // `t1` is bound to the live orchestrator's (repo, branch), so its card carries the task.
  const orchestrators = page.getByRole("region", { name: "Orchestrators" });
  const chip = orchestrators.getByTestId("task-chip").first();
  await expect(chip).toBeVisible();
  await expect(chip).toContainText("Wire the fleet task chip");

  // Tapping the chip navigates to the task — not to the card's own agent link. The chip is a
  // button driving useNavigate precisely so it can win over the surrounding <Link>.
  await chip.click();
  await expect(page).toHaveURL(/\/tasks\/t1$/);
  await expect(page.getByRole("heading", { name: "Wire the fleet task chip" })).toBeVisible();

  // The round trip closes: the task page names the agent working it and links back to its
  // transcript, so a task and the thing running it are never two disconnected screens.
  await expect(page.getByText("Working on it")).toBeVisible();
  const agentLink = page.getByRole("link", { name: /bn:orchestrator/ });
  await expect(agentLink).toBeVisible();
  await agentLink.click();
  await expect(page).toHaveURL(/\/agents\//);
});

test("fleet: an agent with no task says so rather than rendering an empty gap", async ({ page }) => {
  await primeToken(page);
  await page.goto("/fleet");

  // A needs-you card is the surface the board spends on a decision, and "we don't know what this
  // is working on" is part of that decision — so it is stated, not silently omitted.
  const needs = page.getByRole("region", { name: "Needs you" });
  await expect(needs.getByTestId("no-task").first()).toBeVisible();
});

test("fleet: Relaunch runs `bn agent relaunch`, never `bn agent recover`", async ({ page }) => {
  await primeToken(page);
  await page.goto("/fleet");

  const needs = page.getByRole("region", { name: "Needs you" });
  page.once("dialog", (d) => d.accept());
  const [res] = await Promise.all([
    page.waitForResponse((r) => r.url().includes("/relaunch") && r.request().method() === "POST"),
    needs.getByRole("button", { name: /Relaunch/ }).first().click(),
  ]);

  // The stub `bn` echoes its argv, so this asserts the *command the server actually issued*.
  // Relaunch used to shell out to `bn agent recover --yes`, which pushes a branch and opens a PR —
  // a different operation entirely from the word on the button.
  const body = await res.json();
  expect(body.data.args).toContain("agent relaunch");
  expect(body.data.args).not.toContain("recover");
});

test("task page: an unknown id gets a real screen, not a blank one", async ({ page }) => {
  await primeToken(page);
  await page.goto("/tasks/t999");
  await expect(page.getByRole("heading", { name: /No task t999/ })).toBeVisible();
});
