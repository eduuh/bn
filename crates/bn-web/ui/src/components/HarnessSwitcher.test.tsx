// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { HarnessSwitcher } from "./HarnessSwitcher";
import type { HarnessOption } from "../api/types";
import * as writes from "../api/writes";

function wrap(node: ReactNode) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={qc}>{node}</QueryClientProvider>);
}

const both: HarnessOption[] = [
  { name: "copilot", available: true },
  { name: "claude", available: true },
];

const btn = (name: RegExp) => screen.getByRole("button", { name }) as HTMLButtonElement;

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("HarnessSwitcher", () => {
  it("marks the current harness active and non-current available ones switchable", () => {
    wrap(<HarnessSwitcher target="bn:dispatch-501" current="copilot" harnesses={both} />);
    expect(btn(/copilot/).getAttribute("aria-pressed")).toBe("true");
    expect(btn(/copilot/).getAttribute("data-active")).toBe("true");
    expect(btn(/claude/).getAttribute("aria-pressed")).toBe("false");
    expect(btn(/claude/).disabled).toBe(false);
  });

  it("renders an unavailable harness disabled with its reason as a tooltip (no silent drop)", () => {
    const reason = "organization has disabled Claude subscription access";
    wrap(
      <HarnessSwitcher
        target="bn:dispatch-501"
        current="copilot"
        harnesses={[
          { name: "copilot", available: true },
          { name: "claude", available: false, reason },
        ]}
      />,
    );
    expect(btn(/claude/).disabled).toBe(true);
    expect(btn(/claude/).getAttribute("title")).toBe(reason);
  });

  it("confirms then POSTs the relaunch under the chosen harness", async () => {
    const spy = vi.spyOn(writes, "switchHarness").mockResolvedValue({});
    vi.spyOn(window, "confirm").mockReturnValue(true);
    wrap(<HarnessSwitcher target="bn:dispatch-501" current="claude" harnesses={both} />);
    fireEvent.click(btn(/copilot/));
    await waitFor(() => expect(spy).toHaveBeenCalledWith("bn:dispatch-501", "copilot"));
  });

  it("does not switch when the confirm is dismissed", () => {
    const spy = vi.spyOn(writes, "switchHarness").mockResolvedValue({});
    vi.spyOn(window, "confirm").mockReturnValue(false);
    wrap(<HarnessSwitcher target="bn:dispatch-501" current="claude" harnesses={both} />);
    fireEvent.click(btn(/copilot/));
    expect(spy).not.toHaveBeenCalled();
  });

  it("clicking the current harness is a no-op (no confirm, no POST)", () => {
    const spy = vi.spyOn(writes, "switchHarness").mockResolvedValue({});
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    wrap(<HarnessSwitcher target="bn:dispatch-501" current="copilot" harnesses={both} />);
    fireEvent.click(btn(/copilot/));
    expect(confirm).not.toHaveBeenCalled();
    expect(spy).not.toHaveBeenCalled();
  });
});
