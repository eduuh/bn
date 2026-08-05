// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { StartAgentComposer } from "./StartAgentComposer";
import type { ActionItem } from "../api/types";

const BLOCKER: ActionItem = {
  id: "blocker:bn/feat/b1",
  type: "blocker",
  title: "blocked on the api",
  source: "note · bn/feat",
  severity: "urgent",
  repo: "bn",
  branch: "feat",
  link: null,
  detail: null,
  payload: { repo: "bn", branch: "feat", target: "bn:feat" },
  suggested_prompt: "unblock it",
  default_urgency: "urgent",
  created: 5_000,
  stale: false,
};

function wrap(node: ReactNode) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={qc}>{node}</QueryClientProvider>);
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("StartAgentComposer (#522)", () => {
  it("dispatches an urgent item via POST /api/dispatch with the edited prompt", async () => {
    const fetchMock = vi.fn(async () => ({
      ok: true,
      status: 200,
      json: async () => ({ ok: true, command: "dispatch", data: {}, error: null }),
    }));
    vi.stubGlobal("fetch", fetchMock as unknown as typeof fetch);

    wrap(<StartAgentComposer item={BLOCKER} onClose={() => {}} />);

    // The prompt is pre-filled with the item's suggested_prompt and stays editable.
    const prompt = (await screen.findByLabelText(/Prompt for the agent/)) as HTMLTextAreaElement;
    expect(prompt.value).toBe("unblock it");
    fireEvent.change(prompt, { target: { value: "unblock the deploy" } });

    fireEvent.click(screen.getByRole("button", { name: /Dispatch agent/ }));

    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe("/api/dispatch");
    expect(init.method).toBe("POST");
    const body = JSON.parse(init.body as string);
    expect(body).toMatchObject({ repo: "bn", item: "unblock the deploy", harness: "claude", kind: "ship" });
  });

  it("queues a non-urgent item via POST /api/orchestrate/queue", async () => {
    const fetchMock = vi.fn(async () => ({
      ok: true,
      status: 200,
      json: async () => ({ ok: true, command: "queue", data: { repo: "bn", added: [], counts: {} }, error: null }),
    }));
    vi.stubGlobal("fetch", fetchMock as unknown as typeof fetch);

    // Switch to queue mode; a queued item takes the queue endpoint, not dispatch.
    wrap(<StartAgentComposer item={{ ...BLOCKER, default_urgency: "queued" }} onClose={() => {}} />);

    fireEvent.click(screen.getByRole("button", { name: /Add to queue/ }));

    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe("/api/orchestrate/queue");
    expect(init.method).toBe("POST");
  });
});
