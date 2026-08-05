// @vitest-environment jsdom
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import { AgentTerminal } from "./AgentTerminal";

// xterm's DOM renderer needs a couple of browser APIs jsdom omits; stubbing them lets the *real*
// terminal parse + render frames into the DOM (same scaffolding the #496 pane test used).
beforeAll(() => {
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener() {},
    removeListener() {},
    addEventListener() {},
    removeEventListener() {},
    dispatchEvent: () => false,
  }));
  const ctx = {
    measureText: () => ({ width: 8 }),
    fillRect() {},
    clearRect() {},
    fillText() {},
    setTransform() {},
    scale() {},
    createLinearGradient: () => ({ addColorStop() {} }),
    getImageData: () => ({ data: [] }),
  } as unknown as CanvasRenderingContext2D;
  HTMLCanvasElement.prototype.getContext = (() =>
    ctx) as unknown as typeof HTMLCanvasElement.prototype.getContext;
});

/** A stand-in `WebSocket` the test drives: it records `send`s and lets a test push server frames. */
class MockWebSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;
  static instances: MockWebSocket[] = [];

  url: string;
  readyState = MockWebSocket.CONNECTING;
  sent: string[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;

  constructor(url: string) {
    this.url = url;
    MockWebSocket.instances.push(this);
    setTimeout(() => {
      this.readyState = MockWebSocket.OPEN;
      this.onopen?.();
    }, 0);
  }
  send(data: string) {
    this.sent.push(data);
  }
  close() {
    this.readyState = MockWebSocket.CLOSED;
    this.onclose?.();
  }
  /** Deliver a server frame to the component. */
  emit(obj: unknown) {
    this.onmessage?.({ data: JSON.stringify(obj) });
  }
}

const ANSI_FRAME = [
  "\u001b[2J\u001b[H\u001b[36m╭─ bn ────────────╮\u001b[0m",
  "\u001b[36m│\u001b[0m \u001b[32m● building the redesign…\u001b[0m",
  "\u001b[36m│\u001b[0m \u001b[1;33m⠋\u001b[0m running \u001b[31mcargo test\u001b[0m",
  "\u001b[36m╰─────────────────╯\u001b[0m",
  "\u001b[32m✓ 42 tests pass\u001b[0m",
];

beforeEach(() => {
  MockWebSocket.instances = [];
  vi.stubGlobal("WebSocket", MockWebSocket as unknown as typeof WebSocket);
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

async function firstSocket(): Promise<MockWebSocket> {
  await waitFor(() => expect(MockWebSocket.instances.length).toBeGreaterThan(0));
  return MockWebSocket.instances[0];
}

describe("AgentTerminal", () => {
  it("opens a terminal socket to the target and streams a frame as a real terminal", async () => {
    const { container } = render(<AgentTerminal target="bn:x" />);
    const ws = await firstSocket();
    // `:` is percent-encoded in the path (the server decodes it back to `bn:x`).
    expect(decodeURIComponent(ws.url)).toContain("/ws/agents/bn:x/terminal");

    ws.emit({ type: "frame", lines: ANSI_FRAME });

    const rows = container.querySelector(".xterm-rows");
    expect(rows).not.toBeNull();
    await waitFor(() => expect(rows!.textContent).toContain("42 tests pass"));

    const text = rows!.textContent ?? "";
    expect(text).toContain("building the redesign");
    expect(text).toContain("cargo test");
    expect(text).toContain("│"); // box-drawing survives
    // …and nothing raw leaks: no ESC byte, no CSI/SGR remnants.
    expect(container.innerHTML).not.toContain("\u001b");
    expect(text).not.toContain("[2J");
    expect(text).not.toContain("[36m");
  });

  it("shows the live status once the socket opens, and 'window closed' on a gone frame", async () => {
    const { container } = render(<AgentTerminal target="bn:x" />);
    const ws = await firstSocket();
    await waitFor(() =>
      expect(container.querySelector('[data-testid="term-status"]')?.textContent).toBe("live"),
    );

    ws.emit({ type: "gone", reason: "capture_failed" });
    await waitFor(() =>
      expect(container.querySelector('[data-testid="term-status"]')?.textContent).toBe(
        "window closed",
      ),
    );
    expect(container.querySelector('[data-testid="term-gone"]')).not.toBeNull();
  });

  it("take-control reveals the composer and forwards a typed line over the socket", async () => {
    const { container, getByRole } = render(<AgentTerminal target="bn:x" />);
    const ws = await firstSocket();

    fireEvent.click(getByRole("button", { name: /Take control/ }));
    // The composer is immediately ready (no token field — the web token was removed, #523).
    const field = container.querySelector('[data-testid="term-input"]') as HTMLInputElement;
    expect(field).not.toBeNull();
    fireEvent.change(field, { target: { value: "focus on mobile" } });
    fireEvent.submit(field.closest("form")!);

    const inputs = ws.sent.map((s) => JSON.parse(s)).filter((m) => m.type === "input");
    expect(inputs).toHaveLength(1);
    expect(inputs[0].data).toBe("focus on mobile");
    // The field clears after sending.
    await waitFor(() =>
      expect((container.querySelector('[data-testid="term-input"]') as HTMLInputElement).value).toBe(
        "",
      ),
    );
  });
});
