import { describe, expect, it } from "vitest";

import { harnessLabel } from "./cost";

describe("harnessLabel", () => {
  it("gives each harness its product name, matching `bn cost --by harness`", () => {
    expect(harnessLabel("claude")).toBe("Claude Code");
    expect(harnessLabel("copilot")).toBe("Copilot");
  });

  it("keeps an unknown harness under its own name rather than folding it into a known row", () => {
    expect(harnessLabel("codex")).toBe("codex");
  });

  it("renders the no-harness bucket as an em dash", () => {
    // The scheduled ledger dispatches headless and records none. The fold sends "—" for that
    // empty key, so both spellings have to land on the same label.
    expect(harnessLabel("")).toBe("—");
    expect(harnessLabel("—")).toBe("—");
  });
});
