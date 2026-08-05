import { describe, expect, it } from "vitest";
import {
  codeBlocks,
  collectReferences,
  extractUrls,
  prLink,
  shortenUrl,
  structuredSections,
} from "./note";
import type { NoteDetail, NoteFront, NoteItem } from "../api/types";

function item(over: Partial<NoteItem>): NoteItem {
  return { id: "i1", text: "an item", created: "2026-07-13", ...over };
}

function front(over: Partial<NoteFront>): NoteFront {
  return {
    repo: "bn",
    branch: "dispatch-web-session-view-484",
    created: "2026-07-13",
    status: "active",
    tags: [],
    collaborators: [],
    todos: [],
    blockers: [],
    decisions: [],
    research: [],
    collab: [],
    ask: [],
    done_when: [],
    ...over,
  };
}

function note(over: Partial<NoteDetail>): NoteDetail {
  return {
    repo: "bn",
    branch: "dispatch-web-session-view-484",
    front: front({}),
    body: "",
    goal: "",
    ...over,
  };
}

describe("extractUrls", () => {
  it("finds urls and trims trailing sentence punctuation", () => {
    expect(extractUrls("see https://example.com/docs. and https://x.io/a,")).toEqual([
      "https://example.com/docs",
      "https://x.io/a",
    ]);
  });

  it("stops at wrapping parens/brackets, not mid-path", () => {
    expect(extractUrls("(https://example.com/p) [https://y.io/q]")).toEqual([
      "https://example.com/p",
      "https://y.io/q",
    ]);
  });

  it("returns nothing for empty / null / non-url text", () => {
    expect(extractUrls("")).toEqual([]);
    expect(extractUrls(null)).toEqual([]);
    expect(extractUrls("no links here")).toEqual([]);
  });
});

describe("shortenUrl", () => {
  it("drops protocol + trailing slash and caps length", () => {
    expect(shortenUrl("https://github.com/eduuh/bn/")).toBe("github.com/eduuh/bn");
    expect(shortenUrl("https://x.io/" + "a".repeat(80), 20)).toMatch(/…$/);
    expect(shortenUrl("https://x.io/" + "a".repeat(80), 20).length).toBe(20);
  });
});

describe("prLink", () => {
  it("builds an href + #N label from a PR url and carries the note status", () => {
    const pr = prLink(note({ front: front({ pr: "https://github.com/eduuh/bn/pull/484", status: "active" }) }));
    expect(pr).toEqual({ href: "https://github.com/eduuh/bn/pull/484", label: "#484", status: "active" });
  });

  it("labels a bare numeric PR as #N with no href", () => {
    const pr = prLink(note({ front: front({ pr: 484 }) }));
    expect(pr).toEqual({ href: null, label: "#484", status: "active" });
  });

  it("is null when the note records no PR", () => {
    expect(prLink(note({ front: front({ pr: null }) }))).toBeNull();
    expect(prLink(note({ front: front({ pr: "" }) }))).toBeNull();
    expect(prLink(note({}))).toBeNull();
  });
});

describe("collectReferences", () => {
  it("gathers unique urls from body + items + item links, excluding the PR", () => {
    const n = note({
      body: "docs at https://docs.example.com/a and https://docs.example.com/a again",
      front: front({
        pr: "https://github.com/eduuh/bn/pull/484",
        decisions: [item({ text: "chose approach, see https://rfc.example.com/1" })],
        research: [item({ text: "benchmarked", link: "https://bench.example.com/x" })],
        // The PR url appearing again in prose is still excluded.
        collab: [item({ text: "paired — https://github.com/eduuh/bn/pull/484" })],
      }),
    });
    expect(collectReferences(n)).toEqual([
      { url: "https://docs.example.com/a", label: "docs.example.com/a" },
      { url: "https://rfc.example.com/1", label: "rfc.example.com/1" },
      { url: "https://bench.example.com/x", label: "bench.example.com/x" },
    ]);
  });

  it("is empty when the note has no links", () => {
    expect(collectReferences(note({ body: "plain prose, no urls" }))).toEqual([]);
  });
});

describe("codeBlocks", () => {
  it("extracts fenced blocks and drops empty ones", () => {
    const body = [
      "# note",
      "```sh",
      "bn verify",
      "bn gate",
      "```",
      "prose",
      "```",
      "```",
      "```",
      "cargo test --workspace",
      "```",
    ].join("\n");
    expect(codeBlocks(body)).toEqual(["bn verify\nbn gate", "cargo test --workspace"]);
  });

  it("returns nothing without fences", () => {
    expect(codeBlocks("just prose")).toEqual([]);
    expect(codeBlocks(null)).toEqual([]);
  });
});

describe("structuredSections", () => {
  it("returns non-empty sections in canonical order with open todos only", () => {
    const f = front({
      done_when: [item({ id: "w1", text: "tests pass" })],
      todos: [item({ id: "t1", text: "open one" }), item({ id: "t2", text: "closed one", done: true })],
      decisions: [item({ id: "d1", text: "a decision" })],
      // blockers / research / collab / ask empty → omitted.
    });
    const secs = structuredSections(f);
    expect(secs.map((s) => s.key)).toEqual(["done_when", "todos", "decisions"]);
    const todos = secs.find((s) => s.key === "todos")!;
    expect(todos.items.map((i) => i.id)).toEqual(["t1"]);
    expect(todos.checks).toBe(true);
    expect(secs.find((s) => s.key === "decisions")!.checks).toBe(false);
  });

  it("is empty when every section is empty", () => {
    expect(structuredSections(front({}))).toEqual([]);
  });
});
