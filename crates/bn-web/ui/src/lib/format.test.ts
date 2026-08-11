import { describe, expect, it } from "vitest";

import { fmtAic, fmtAicCompact } from "./format";

describe("fmtAicCompact", () => {
  it("distinguishes an absent figure from a real zero", () => {
    expect(fmtAicCompact(null)).toBe("—");
    expect(fmtAicCompact(Number.NaN)).toBe("—");
    expect(fmtAicCompact(0)).toBe("0.00");
  });

  it("keeps 2dp below a thousand, where the cents are still the number", () => {
    expect(fmtAicCompact(48.512)).toBe("48.51");
    expect(fmtAicCompact(999.994)).toBe("999.99");
  });

  it("switches to k / M above it, so a list row never truncates mid-number", () => {
    expect(fmtAicCompact(1_000)).toBe("1.0k");
    expect(fmtAicCompact(2_770.71)).toBe("2.8k");
    expect(fmtAicCompact(147_396.4962)).toBe("147.4k");
    expect(fmtAicCompact(1_204_000)).toBe("1.20M");
  });

  it("handles a negative figure without losing its sign", () => {
    expect(fmtAicCompact(-2_500)).toBe("-2.5k");
  });

  it("says the same number as `fmtAic`, at less precision", () => {
    expect(fmtAic(147_396.4962)).toBe("147,396.50");
    expect(fmtAicCompact(147_396.4962)).toBe("147.4k");
  });
});
