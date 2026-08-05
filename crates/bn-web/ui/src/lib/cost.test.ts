import { describe, it, expect } from "vitest";
import {
  buildStackModel,
  categoryColor,
  categoryLabel,
  presentCategories,
  stackForDraw,
} from "./cost";
import type { DayCategory } from "../api/types";

function cell(over: Partial<DayCategory>): DayCategory {
  return {
    day: "2026-07-01",
    category: "coding",
    sessions: 1,
    input_tokens: 0,
    cache_read_tokens: 0,
    output_tokens: 0,
    aic: null,
    cost_usd: null,
    ...over,
  };
}

describe("category palette", () => {
  it("title-cases known classes and passes unknowns through", () => {
    expect(categoryLabel("orchestration")).toBe("Orchestration");
    expect(categoryLabel("unattributed")).toBe("Unattributed");
    expect(categoryLabel("mystery")).toBe("mystery");
  });

  it("colours known classes stably and cycles a fallback for unknowns", () => {
    expect(categoryColor("coding")).toBe("#3fb950");
    // Unknown → fallback palette, never undefined.
    expect(categoryColor("mystery", 0)).toMatch(/^#/);
    expect(categoryColor("mystery", 0)).not.toBe(categoryColor("mystery", 1));
  });
});

describe("presentCategories", () => {
  it("orders known classes first then unknowns alphabetically", () => {
    const cells = [
      cell({ category: "zeta" }),
      cell({ category: "coding" }),
      cell({ category: "orchestration" }),
      cell({ category: "alpha" }),
    ];
    expect(presentCategories(cells)).toEqual(["orchestration", "coding", "alpha", "zeta"]);
  });
});

describe("buildStackModel", () => {
  it("cross-tabs day × category into a values matrix, summing duplicate cells", () => {
    const cells = [
      cell({ day: "2026-07-02", category: "coding", output_tokens: 100 }),
      cell({ day: "2026-07-01", category: "coding", output_tokens: 10 }),
      cell({ day: "2026-07-01", category: "coding", output_tokens: 5 }),
      cell({ day: "2026-07-01", category: "scheduled", output_tokens: 20 }),
    ];
    const m = buildStackModel(cells, "output_tokens");
    expect(m.days).toEqual(["2026-07-01", "2026-07-02"]);
    expect(m.categories).toEqual(["coding", "scheduled"]);
    // coding: day0 = 10+5 = 15, day1 = 100; scheduled: day0 = 20, day1 = 0.
    expect(m.values).toEqual([
      [15, 100],
      [20, 0],
    ]);
  });

  it("treats null costs as 0 when the metric is a cost unit", () => {
    const cells = [
      cell({ day: "2026-07-01", category: "coding", cost_usd: null }),
      cell({ day: "2026-07-01", category: "coding", cost_usd: 2.5 }),
    ];
    const m = buildStackModel(cells, "cost_usd");
    expect(m.values).toEqual([[2.5]]);
  });
});

describe("stackForDraw", () => {
  it("cumulates bottom-up and returns series tallest-first for correct overpaint", () => {
    // Two categories over one day: values [[3],[5]] → cumulative [3, 8].
    const { order, cumulative } = stackForDraw([[3], [5]]);
    // Draw order is reversed (highest cumulative first) so the bottom band paints last on top.
    expect(order).toEqual([1, 0]);
    expect(cumulative).toEqual([[8], [3]]);
  });

  it("handles the empty matrix without throwing", () => {
    expect(stackForDraw([])).toEqual({ order: [], cumulative: [] });
  });
});
