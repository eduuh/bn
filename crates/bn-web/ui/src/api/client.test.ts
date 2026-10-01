import { describe, expect, it } from "vitest";
import { agentDetailQuery, noteFileQuery, taskQuery } from "./client";

describe("detail query retry policy", () => {
  it("does not retry missing agent detail fetches", () => {
    expect(agentDetailQuery("missing-session").retry).toBe(0);
  });

  it("does not retry missing task detail fetches", () => {
    expect(taskQuery("t99999999").retry).toBe(0);
  });

  it("does not fetch a note file until a real path is selected", () => {
    expect(noteFileQuery("bn", "branch", "").enabled).toBe(false);
  });
});
