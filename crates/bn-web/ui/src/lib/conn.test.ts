import { describe, it, expect } from "vitest";
import { classifyConn, DOWN_AFTER_FAILURES } from "./conn";

describe("classifyConn", () => {
  it("an open stream is always live, regardless of a stale failure run", () => {
    expect(classifyConn(true, 0)).toBe("open");
    expect(classifyConn(true, 99)).toBe("open");
  });

  it("a just-dropped stream is a transient blip within the grace", () => {
    expect(classifyConn(false, 0)).toBe("lost");
    expect(classifyConn(false, DOWN_AFTER_FAILURES - 1)).toBe("lost");
  });

  it("escalates to an honest outage once probes fail past the grace", () => {
    expect(classifyConn(false, DOWN_AFTER_FAILURES)).toBe("down");
    expect(classifyConn(false, DOWN_AFTER_FAILURES + 5)).toBe("down");
  });
});
