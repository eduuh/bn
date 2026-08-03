// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { RepoSelect } from "./RepoSelect";

const env = (command: string, data: unknown) => ({
  ok: true,
  json: async () => ({ ok: true, command, data }),
});

const REPOS = { repos: [], known: ["bn", "personal-notes", "webapp"] };

function wrap(node: ReactNode) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={qc}>{node}</QueryClientProvider>);
}

beforeEach(() => {
  // cmdk relies on ResizeObserver + scrollIntoView, which jsdom doesn't implement (same shim as
  // CommandPalette.test.tsx).
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
      if (String(url).startsWith("/api/repos")) return env("repos", REPOS) as unknown as Response;
      throw new Error(`unexpected fetch: ${url}`);
    }),
  );
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("RepoSelect", () => {
  it("offers every repo bn knows about, not just the ones with live agents", async () => {
    // The whole reason this replaced the old datalists: a managed repo with no running agent used
    // to be un-pickable, because the options came from the fleet.
    wrap(<RepoSelect value="" onChange={() => {}} />);
    fireEvent.click(screen.getByRole("button"));
    await waitFor(() => expect(screen.getByText("personal-notes")).toBeTruthy());
    expect(screen.getByText("bn")).toBeTruthy();
    expect(screen.getByText("webapp")).toBeTruthy();
  });

  it("emits the picked repo and closes", async () => {
    const onChange = vi.fn();
    wrap(<RepoSelect value="" onChange={onChange} />);
    fireEvent.click(screen.getByRole("button"));
    fireEvent.click(await screen.findByText("webapp"));
    expect(onChange).toHaveBeenCalledWith("webapp");
    await waitFor(() => expect(screen.queryByPlaceholderText("Search repos…")).toBeNull());
  });

  it("filters as you type", async () => {
    wrap(<RepoSelect value="" onChange={() => {}} />);
    fireEvent.click(screen.getByRole("button"));
    const input = await screen.findByPlaceholderText("Search repos…");
    fireEvent.change(input, { target: { value: "syd" } });
    await waitFor(() => expect(screen.queryByText("bn")).toBeNull());
    expect(screen.getByText("webapp")).toBeTruthy();
  });

  it("keeps showing a value the server no longer lists", async () => {
    // A renamed or removed repo must stay visible so you can see what the form is holding —
    // silently reading as empty would hide a wrong value rather than surface it.
    wrap(<RepoSelect value="retired-repo" onChange={() => {}} />);
    expect(screen.getByRole("button").textContent).toContain("retired-repo");
    fireEvent.click(screen.getByRole("button"));
    await waitFor(() => expect(screen.getAllByText("retired-repo").length).toBeGreaterThan(0));
  });

  it("offers 'any repo' only when the caller allows clearing", async () => {
    // Filters mean "all repos" by empty; a field that starts work means "unanswered", and offering
    // to clear it there would invite dispatching without a repo.
    const { unmount } = wrap(<RepoSelect value="bn" onChange={() => {}} allowClear />);
    fireEvent.click(screen.getByRole("button"));
    expect(await screen.findByText("any repo")).toBeTruthy();
    unmount();

    wrap(<RepoSelect value="bn" onChange={() => {}} required />);
    fireEvent.click(screen.getByRole("button"));
    await screen.findByPlaceholderText("Search repos…");
    expect(screen.queryByText("any repo")).toBeNull();
  });
});
