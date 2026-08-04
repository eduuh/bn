// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useState } from "react";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { CommandPalette } from "./CommandPalette";
import type { ActionItem, SearchView } from "../api/types";

const env = (command: string, data: unknown) => ({
  ok: true,
  json: async () => ({ ok: true, command, data }),
});

/** The embedded "work on" action for the task result — what the composer consumes. */
const TODO_ACTION: ActionItem = {
  id: "todo:bn/x/t1",
  type: "todo",
  title: "Wire the search palette",
  source: "note · bn/x",
  severity: "info",
  repo: "bn",
  branch: "x",
  link: null,
  detail: null,
  payload: { repo: "bn", branch: "x", target: "bn:x" },
  suggested_prompt: "pick up the palette todo",
  default_urgency: "queued",
  created: 1000,
  stale: false,
};

const RESULTS: SearchView = {
  query: "palette",
  count: 2,
  results: [
    {
      id: "action:todo:bn/x/t1",
      type: "todo",
      title: "Wire the search palette",
      subtitle: "note · bn/x",
      repo: "bn",
      branch: "x",
      link: null,
      route: null,
      action: TODO_ACTION,
      score: 140,
    },
    {
      id: "note:bn/x",
      type: "note",
      title: "bn/x",
      subtitle: "note · active",
      repo: "bn",
      branch: "x",
      link: null,
      route: "/notes",
      action: null,
      score: 100,
    },
  ],
};

/** Harness that owns `open` so `onOpenChange(false)` actually closes the palette (as the app does). */
function Harness() {
  const [open, setOpen] = useState(true);
  return <CommandPalette open={open} onOpenChange={setOpen} />;
}

function wrap() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>
        <Harness />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  // cmdk relies on ResizeObserver + scrollIntoView, which jsdom doesn't implement.
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
  if (!Element.prototype.scrollIntoView) {
    Element.prototype.scrollIntoView = () => {};
  }
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string | URL) => {
      const u = String(url);
      if (u.startsWith("/api/search")) return env("search", RESULTS) as unknown as Response;
      throw new Error(`unexpected fetch: ${u}`);
    }),
  );
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("Command palette (#565)", () => {
  it("prompts before a query, then renders ranked typed results", async () => {
    wrap();
    // Empty box → every destination, plus the guidance prompt. No fetch.
    expect(screen.getByText(/type to search across everything bn tracks/)).toBeTruthy();
    expect(screen.getByText("Destinations")).toBeTruthy();

    fireEvent.change(screen.getByPlaceholderText(/Search tasks, issues, notes/), {
      target: { value: "palette" },
    });

    await waitFor(() => expect(screen.getByText("Wire the search palette")).toBeTruthy());
    // Both the actionable task and the navigational note render.
    expect(screen.getByText("bn/x")).toBeTruthy();
    // The task offers "Work on"; the note offers "Jump".
    expect(screen.getByText("Work on")).toBeTruthy();
    expect(screen.getByText("Jump")).toBeTruthy();
  });

  it("reaches a screen by name, even when no data row matches", async () => {
    // The regression this guards: the palette used to search *data* only, so typing the name of a
    // screen with no same-named data row ("cost", "console") dead-ended on "No results." — the most
    // common thing anyone types into a palette, and the only fast navigation on a phone.
    wrap();
    fireEvent.change(screen.getByPlaceholderText(/Search tasks, issues, notes/), {
      target: { value: "cost" },
    });

    expect(screen.getByText("Go to")).toBeTruthy();
    expect(screen.getByText("Cost")).toBeTruthy();
    expect(screen.getByText("/insights?tab=cost")).toBeTruthy();
    expect(screen.queryByText("No results.")).toBeNull();
  });

  it("matches a destination by synonym, not just its label", async () => {
    wrap();
    fireEvent.change(screen.getByPlaceholderText(/Search tasks, issues, notes/), {
      target: { value: "money" },
    });
    expect(screen.getByText("Cost")).toBeTruthy();
  });

  it("acts on a task result by opening the reused Start-agent composer", async () => {
    wrap();
    fireEvent.change(screen.getByPlaceholderText(/Search tasks, issues, notes/), {
      target: { value: "palette" },
    });
    await waitFor(() => expect(screen.getByText("Wire the search palette")).toBeTruthy());

    fireEvent.click(screen.getByText("Wire the search palette"));

    // The palette closes and the composer opens, pre-filled from the embedded action.
    await waitFor(() => expect(screen.getByText("Prompt for the agent")).toBeTruthy());
    expect(screen.queryByPlaceholderText(/Search tasks, issues, notes/)).toBeNull();
    expect(screen.getByDisplayValue("pick up the palette todo")).toBeTruthy();
  });
});
