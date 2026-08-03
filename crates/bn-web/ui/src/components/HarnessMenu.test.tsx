// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { HarnessMenu } from "./HarnessMenu";
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

const trigger = () => screen.getByRole("button", { name: /claude/ });
const item = (name: RegExp) => screen.getByRole("menuitem", { name }) as HTMLButtonElement;

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("HarnessMenu", () => {
  it("shows only the running harness until opened", () => {
    wrap(<HarnessMenu target="bn:x" current="claude" harnesses={both} />);
    expect(trigger()).toBeTruthy();
    expect(screen.queryByRole("menu")).toBeNull();
    expect(screen.queryByRole("menuitem", { name: /copilot/ })).toBeNull();
  });

  it("opens the alternatives on click", () => {
    wrap(<HarnessMenu target="bn:x" current="claude" harnesses={both} />);
    fireEvent.click(trigger());
    expect(screen.getByRole("menu")).toBeTruthy();
    expect(item(/copilot/)).toBeTruthy();
    expect(item(/claude/).getAttribute("aria-checked")).toBe("true");
  });

  it("confirms then POSTs the relaunch under the chosen harness", async () => {
    const spy = vi.spyOn(writes, "switchHarness").mockResolvedValue({});
    vi.spyOn(window, "confirm").mockReturnValue(true);
    wrap(<HarnessMenu target="bn:dispatch-501" current="claude" harnesses={both} />);
    fireEvent.click(trigger());
    fireEvent.click(item(/copilot/));
    await waitFor(() => expect(spy).toHaveBeenCalledWith("bn:dispatch-501", "copilot"));
  });

  it("does not switch when the confirm is dismissed", () => {
    const spy = vi.spyOn(writes, "switchHarness").mockResolvedValue({});
    vi.spyOn(window, "confirm").mockReturnValue(false);
    wrap(<HarnessMenu target="bn:x" current="claude" harnesses={both} />);
    fireEvent.click(trigger());
    fireEvent.click(item(/copilot/));
    expect(spy).not.toHaveBeenCalled();
  });

  it("picking the running harness is a no-op", () => {
    const spy = vi.spyOn(writes, "switchHarness").mockResolvedValue({});
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    wrap(<HarnessMenu target="bn:x" current="claude" harnesses={both} />);
    fireEvent.click(trigger());
    fireEvent.click(item(/claude/));
    expect(confirm).not.toHaveBeenCalled();
    expect(spy).not.toHaveBeenCalled();
  });

  it("renders an unavailable harness disabled with its reason (no silent drop)", () => {
    const reason = "organization has disabled Claude subscription access";
    wrap(
      <HarnessMenu
        target="bn:x"
        current="claude"
        harnesses={[
          { name: "claude", available: true },
          { name: "copilot", available: false, reason },
        ]}
      />,
    );
    fireEvent.click(trigger());
    expect(item(/copilot/).disabled).toBe(true);
    expect(item(/copilot/).getAttribute("title")).toBe(reason);
  });

  it("closes on Escape", () => {
    wrap(<HarnessMenu target="bn:x" current="claude" harnesses={both} />);
    fireEvent.click(trigger());
    expect(screen.getByRole("menu")).toBeTruthy();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("degrades to a plain label when there is nothing to switch to", () => {
    wrap(<HarnessMenu target="bn:x" current="claude" harnesses={[{ name: "claude", available: true }]} />);
    expect(screen.queryByRole("button")).toBeNull();
    expect(screen.getByText("claude")).toBeTruthy();
  });

  it("renders nothing when the harness is unknown", () => {
    const { container } = wrap(<HarnessMenu target="bn:x" current={undefined} harnesses={both} />);
    expect(container.textContent).toBe("");
  });
});
