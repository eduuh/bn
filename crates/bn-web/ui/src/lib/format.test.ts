import { describe, expect, it } from "vitest";

import { fmtAic, fmtAicCompact, fmtCacheReadWrite, fmtTokens } from "./format";

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

describe("fmtTokens", () => {
  it("reaches billions, which cached agent sessions actually do", () => {
    // Every turn re-reads the whole cached prefix, so a month of cache-read totals passes 10^9;
    // without this tier it rendered as `4839.35M`.
    expect(fmtTokens(4_839_350_000)).toBe("4.84B");
    expect(fmtTokens(999_999_999)).toBe("1000.00M");
  });

  it("keeps the smaller tiers unchanged", () => {
    expect(fmtTokens(840)).toBe("840");
    expect(fmtTokens(3_200)).toBe("3.2k");
    expect(fmtTokens(1_450_000)).toBe("1.45M");
  });
});

describe("fmtCacheReadWrite", () => {
  it("keeps reads and writes apart, since they move the bill in opposite directions", () => {
    expect(fmtCacheReadWrite(3_900_000, 412_000)).toBe("3.90M / 412.0k");
  });

  it("still shows a zero write column rather than hiding the half that was not measured", () => {
    // Runs ingested before cache writes were captured report 0 — visibly zero, not absent.
    expect(fmtCacheReadWrite(1_200_000, 0)).toBe("1.20M / 0");
  });
});
