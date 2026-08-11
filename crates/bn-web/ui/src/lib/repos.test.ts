import { describe, expect, it } from "vitest";

import {
  filterRepos,
  matchesFilter,
  matchesQuery,
  repoFilterCounts,
  type FilterableRepo,
} from "./repos";

const repo = (over: Partial<FilterableRepo> & { repo: string }): FilterableRepo => ({
  active_agents: 0,
  setup: { needs_setup: false },
  pool: { enabled: false },
  ...over,
});

const BN = repo({ repo: "bn", active_agents: 2, pool: { enabled: true } });
const WEB = repo({ repo: "acme-web", setup: { needs_setup: true } });
const DOCS = repo({ repo: "Docs" });
const ALL = [BN, WEB, DOCS];

describe("matchesFilter", () => {
  it("keeps everything under `all`", () => {
    expect(ALL.every((r) => matchesFilter(r, "all"))).toBe(true);
  });

  it("narrows to repos with a live agent, missing scripts, or a pool", () => {
    expect(ALL.filter((r) => matchesFilter(r, "active"))).toEqual([BN]);
    expect(ALL.filter((r) => matchesFilter(r, "setup"))).toEqual([WEB]);
    expect(ALL.filter((r) => matchesFilter(r, "pooled"))).toEqual([BN]);
  });

  it("treats a missing setup block as not needing setup", () => {
    expect(matchesFilter(repo({ repo: "x", setup: null }), "setup")).toBe(false);
    expect(matchesFilter(repo({ repo: "x", setup: undefined }), "setup")).toBe(false);
  });
});

describe("matchesQuery", () => {
  it("matches a case-insensitive substring, not just a prefix", () => {
    expect(matchesQuery(WEB, "WEB")).toBe(true);
    expect(matchesQuery(DOCS, "docs")).toBe(true);
    expect(matchesQuery(BN, "web")).toBe(false);
  });

  it("matches everything for an empty or whitespace query", () => {
    expect(ALL.every((r) => matchesQuery(r, ""))).toBe(true);
    expect(ALL.every((r) => matchesQuery(r, "   "))).toBe(true);
  });
});

describe("filterRepos", () => {
  it("applies both the chip and the search box, preserving the server's order", () => {
    expect(filterRepos(ALL, "all", "")).toEqual(ALL);
    expect(filterRepos(ALL, "all", "c")).toEqual([WEB, DOCS]);
    expect(filterRepos(ALL, "active", "bn")).toEqual([BN]);
    expect(filterRepos(ALL, "active", "docs")).toEqual([]);
  });
});

describe("repoFilterCounts", () => {
  it("counts against the search results, not the whole inventory", () => {
    expect(repoFilterCounts(ALL, "")).toEqual({ all: 3, active: 1, setup: 1, pooled: 1 });
    expect(repoFilterCounts(ALL, "acme")).toEqual({ all: 1, active: 0, setup: 1, pooled: 0 });
  });
});
