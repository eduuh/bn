// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { AgentActivity } from "./AgentActivity";
import type { ActivityEntry, ActivityView } from "../api/types";

function wrap(node: ReactNode) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={qc}>{node}</QueryClientProvider>);
}

/** Stub `/api/agents/:id/activity` with a success envelope carrying `view`. */
function serve(view: Partial<ActivityView>) {
  const data: ActivityView = {
    session_id: "sess-1",
    harness: "claude",
    entries: [],
    total: 0,
    count: 0,
    ...view,
  };
  vi.stubGlobal(
    "fetch",
    vi.fn(async () =>
      new Response(JSON.stringify({ ok: true, command: "agent_activity", data, error: null }), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    ),
  );
}

const entry = (e: Partial<ActivityEntry>): ActivityEntry => ({
  kind: "agent",
  ts: 1_785_837_600,
  text: "",
  tools: [],
  summary: "",
  ...e,
});

beforeEach(() => {
  // jsdom has no layout, so the feed's scroll-follow effect reads zeroes; harmless but noisy.
  Element.prototype.scrollTo = vi.fn();
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("AgentActivity", () => {
  it("renders prompts and agent prose as markdown, labelled by speaker", async () => {
    serve({
      total: 2,
      count: 2,
      entries: [
        entry({ kind: "user", text: "remove the cockpit" }),
        entry({ kind: "agent", text: "Cockpit **removed**." }),
      ],
    });
    wrap(<AgentActivity sessionId="sess-1" />);

    await waitFor(() => expect(screen.getByText("remove the cockpit")).toBeTruthy());
    expect(screen.getByText("you")).toBeTruthy();
    expect(screen.getByText("agent")).toBeTruthy();
    // The agent's markdown is rendered, not printed as literal asterisks. `Markdown` lazy-loads
    // its renderer (showing the raw source meanwhile), so this is the async assertion.
    //
    // The explicit timeout is the point: the fallback renders the *raw source*, so until the chunk
    // lands this query finds a plain text node and the tag assertion fails rather than retrying
    // against nothing. That chunk is react-markdown + remark-gfm — the heaviest dependency in the
    // bundle — and transforming it cold on a loaded CI runner comfortably exceeds the default 1s
    // budget, which made this the one test in the suite that failed on CI and passed everywhere
    // else. 5s is still far below the point where a genuinely broken renderer would look healthy.
    expect((await screen.findByText("removed", {}, { timeout: 5000 })).tagName).toBe("STRONG");
  });

  it("collapses a tool run behind its summary and expands it on click", async () => {
    serve({
      total: 1,
      count: 1,
      entries: [
        entry({
          kind: "tools",
          summary: "3 tool calls · Read ×2, Bash",
          tools: [
            { name: "Read", target: "src/a.rs" },
            { name: "Read", target: "src/b.rs" },
            { name: "Bash", target: "Run the tests" },
          ],
        }),
      ],
    });
    wrap(<AgentActivity sessionId="sess-1" />);

    const toggle = await screen.findByRole("button", { name: /3 tool calls/ });
    // Collapsed by default — the prose either side is what you're reading.
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByText("src/a.rs")).toBeNull();

    fireEvent.click(toggle);
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByText("src/a.rs")).toBeTruthy();
    expect(screen.getByText("Run the tests")).toBeTruthy();
  });

  it("says how much of a long session is on screen", async () => {
    serve({
      total: 312,
      count: 1,
      entries: [entry({ kind: "agent", text: "still going" })],
    });
    wrap(<AgentActivity sessionId="sess-1" />);
    await waitFor(() => expect(screen.getByText(/last 1 of 312/)).toBeTruthy());
  });

  it("explains a missing transcript instead of rendering an empty box", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        new Response(
          JSON.stringify({
            ok: false,
            command: "agent_activity",
            data: null,
            error: { code: "no_transcript", message: "no transcript for session 'sess-1'" },
          }),
          { status: 200, headers: { "content-type": "application/json" } },
        ),
      ),
    );
    wrap(<AgentActivity sessionId="sess-1" />);
    await waitFor(() => expect(screen.getByTestId("activity-error")).toBeTruthy());
    expect(screen.getByText(/No transcript for this session/)).toBeTruthy();
  });

  it("reads back a session that has started but produced nothing yet", async () => {
    serve({ total: 0, count: 0, entries: [] });
    wrap(<AgentActivity sessionId="sess-1" />);
    await waitFor(() => expect(screen.getByText(/it's just started/)).toBeTruthy());
  });
});
