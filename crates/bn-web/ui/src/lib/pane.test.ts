import { describe, expect, it } from "vitest";
import { lastPaneLine, stripAnsi } from "./pane";

describe("stripAnsi", () => {
  it("removes SGR colour escapes but keeps the text and box-drawing", () => {
    const out = stripAnsi("\u001b[31mred\u001b[0m \u2502 \u2500\u2500 box \u25CF");
    expect(out).toBe("red │ ── box ●");
    expect(out).not.toContain("\u001b");
    expect(out).not.toContain("[31m");
    expect(out).not.toContain("[0m");
  });

  it("removes cursor-movement and OSC sequences", () => {
    const out = stripAnsi("\u001b[2J\u001b[H\u001b]0;title\u0007done\u001b[1;5H");
    expect(out).toBe("done");
    expect(out).not.toMatch(/\u001b/);
  });
});

describe("lastPaneLine", () => {
  it("returns the newest non-empty line, escape-stripped", () => {
    const lines = ["\u001b[32m● building\u001b[0m", "\u001b[36m✓ 42 tests pass\u001b[0m", "   "];
    expect(lastPaneLine(lines)).toBe("✓ 42 tests pass");
  });

  it("returns empty string when there is nothing to show", () => {
    expect(lastPaneLine([])).toBe("");
    expect(lastPaneLine(["", "  ", "\t"])).toBe("");
  });

  it("truncates a long line to a one-line glance", () => {
    const out = lastPaneLine(["x".repeat(200)], 20);
    expect(out).toHaveLength(20);
    expect(out.endsWith("…")).toBe(true);
  });
});
