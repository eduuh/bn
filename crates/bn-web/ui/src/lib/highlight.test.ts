import { describe, expect, it } from "vitest";
import { grammarFor, highlightCode, languageOf } from "./highlight";

describe("languageOf", () => {
  it("reads the info string out of react-markdown's language- class", () => {
    expect(languageOf("language-rust")).toBe("rust");
    expect(languageOf("hljs language-TypeScript")).toBe("typescript");
    expect(languageOf("language-shell-session")).toBe("shell-session");
  });

  it("is undefined for a fence with no info string", () => {
    expect(languageOf(undefined)).toBeUndefined();
    expect(languageOf("")).toBeUndefined();
    expect(languageOf("some-other-class")).toBeUndefined();
  });
});

describe("grammarFor", () => {
  it("resolves the spellings people actually write", () => {
    expect(grammarFor("sh")).toBe("bash");
    expect(grammarFor("zsh")).toBe("bash");
    expect(grammarFor("ts")).toBe("typescript");
    expect(grammarFor("tsx")).toBe("typescript");
    expect(grammarFor("yml")).toBe("yaml");
    expect(grammarFor("patch")).toBe("diff");
    expect(grammarFor("toml")).toBe("ini");
  });

  it("passes through a grammar that is registered under its own name", () => {
    expect(grammarFor("rust")).toBe("rust");
    expect(grammarFor("diff")).toBe("diff");
  });

  it("is undefined for a language we don't ship, so the block stays plain", () => {
    expect(grammarFor("brainfuck")).toBeUndefined();
    expect(grammarFor(undefined)).toBeUndefined();
  });
});

describe("highlightCode", () => {
  it("marks up a known language", () => {
    const html = highlightCode('let x = "hi";', "rust");
    expect(html).toContain("hljs-");
    expect(html).toContain("hi");
  });

  it("returns null rather than guessing when the fence names nothing we ship", () => {
    expect(highlightCode("let x = 1", undefined)).toBeNull();
    expect(highlightCode("let x = 1", "brainfuck")).toBeNull();
  });

  it("tags diff additions and deletions, which is what plans are full of", () => {
    const html = highlightCode("--- a\n+++ b\n-gone\n+added", "diff") ?? "";
    expect(html).toContain("hljs-deletion");
    expect(html).toContain("hljs-addition");
  });

  it("escapes the source, so a note body cannot inject markup through a code fence", () => {
    const html = highlightCode('let s = "<img src=x onerror=alert(1)>";', "rust") ?? "";
    expect(html).not.toContain("<img");
    expect(html).toContain("&lt;img");
  });

  it("escapes even when the language is one whose grammar knows about tags", () => {
    // The xml grammar wraps the tag in its own spans, so the escaped brackets aren't adjacent to
    // the name — what matters is that no live tag survives and the brackets are entities.
    const html = highlightCode("<script>alert(1)</script>", "xml") ?? "";
    expect(html).not.toContain("<script");
    expect(html).toContain("&lt;");
    expect(html).toContain("&gt;");
    // Every remaining raw `<` opens a span highlight.js added itself, never note content.
    expect(html.match(/<(?!\/?span)/g)).toBeNull();
  });
});
