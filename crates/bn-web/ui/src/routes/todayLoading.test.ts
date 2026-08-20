import { describe, expect, it } from "vitest";

import { todayRouteLoading } from "./todayLoading";

describe("todayRouteLoading", () => {
  it("waits for the route's own documents", () => {
    expect(todayRouteLoading({ goalsLoading: true })).toBe(true);
  });

  it("does not hold the page on slower supporting reads", () => {
    expect(todayRouteLoading({ goalsLoading: false })).toBe(false);
  });
});
