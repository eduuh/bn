// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import type { ReactNode } from "react";
import { Handoff } from "./Handoff";
import * as writes from "../api/writes";

/** Envelope helper — bn reads/writes unwrap `{ ok, data }`. */
const env = (data: unknown) => ({
  ok: true,
  json: async () => ({ ok: true, command: "handoff", data }),
});

const NOTES = {
  branches: [
    { repo: "bn", branch: "dispatch-web-handoff-509", path: "", status: "active", todos: [], blockers: [] },
  ],
};

const HANDOFF = {
  handoff: {
    repo: "bn",
    branch: "dispatch-web-handoff-509",
    written: "2026-07-13",
    state: { goal: "Ship the handoff viewer.", progress: [], open_todos: [], blockers: [] },
    narrative: "Composed from the note keeps one source of truth.",
    decisions: { made: [], deferred: ["whether to push on close"] },
    open_questions: ["does the view need a filter?"],
    priority_queue: ["wire the route", "add the view"],
  },
  markdown: "# Handoff — bn / dispatch-web-handoff-509\n\n## State\n\n- Goal: Ship the handoff viewer.\n",
  rendered: "Handoff — bn / dispatch-web-handoff-509",
};

function wrap(node: ReactNode) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>{node}</MemoryRouter>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string | URL) => {
      const u = String(url);
      if (u.startsWith("/api/notes")) return env(NOTES) as unknown as Response;
      if (u.startsWith("/api/handoff")) return env(HANDOFF) as unknown as Response;
      throw new Error(`unexpected fetch: ${u}`);
    }),
  );
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("Handoff view", () => {
  it("renders the selected branch's packet markdown and prefills the author layers", async () => {
    wrap(<Handoff />);
    // The rendered markdown heading (via react-markdown) appears once the packet loads.
    await waitFor(() => expect(screen.getByText(/Goal: Ship the handoff viewer\./)).toBeTruthy());
    // The narrative textarea is prefilled from the loaded packet.
    const narrative = screen.getByPlaceholderText(/A few sentences/) as HTMLTextAreaElement;
    expect(narrative.value).toContain("one source of truth");
  });

  it("POSTs the edited packet on Save (open write, no token)", async () => {
    const spy = vi.spyOn(writes, "saveHandoff").mockResolvedValue(HANDOFF as never);
    wrap(<Handoff />);
    const narrative = (await screen.findByPlaceholderText(/A few sentences/)) as HTMLTextAreaElement;
    fireEvent.change(narrative, { target: { value: "Updated from the web." } });
    fireEvent.click(screen.getByRole("button", { name: /Save handoff/ }));
    await waitFor(() =>
      expect(spy).toHaveBeenCalledWith(
        expect.objectContaining({
          repo: "bn",
          branch: "dispatch-web-handoff-509",
          narrative: "Updated from the web.",
          priority_queue: ["wire the route", "add the view"],
          deferred: ["whether to push on close"],
          questions: ["does the view need a filter?"],
        }),
      ),
    );
  });
});
