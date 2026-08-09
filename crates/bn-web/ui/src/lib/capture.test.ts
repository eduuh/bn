import { describe, expect, it } from "vitest";

import {
  attachmentResolver,
  draftIsEmpty,
  EMPTY_DRAFT,
  extractUrls,
  inlineAttachmentNames,
  looksLikeIssueUrl,
  uniqueName,
} from "./capture";

describe("attachmentResolver", () => {
  const resolve = attachmentResolver("t42");

  it("points a bare file name at this task's attachment endpoint", () => {
    // Without this the browser resolves `shot.png` against `/tasks/t42` and every pasted
    // screenshot renders as a broken image.
    expect(resolve("shot.png")).toBe("/api/tasks/t42/attach/shot.png");
    expect(resolve(" trace.log ")).toBe("/api/tasks/t42/attach/trace.log");
  });

  it("leaves anything that already resolves alone", () => {
    // A doc may legitimately embed an external image; rewriting it would break the doc.
    for (const src of [
      "https://example.com/a.png",
      "http://example.com/a.png",
      "data:image/png;base64,aGk=",
      "/api/tasks/t9/attach/other.png",
      "nested/path.png",
      "",
    ]) {
      expect(resolve(src)).toBe(src);
    }
  });

  it("escapes a name that would otherwise change the path", () => {
    expect(resolve("a b.png")).toBe("/api/tasks/t42/attach/a%20b.png");
    expect(resolve("..%2Fetc")).toBe("/api/tasks/t42/attach/..%252Fetc");
  });
});

describe("inlineAttachmentNames", () => {
  it("rewrites the editor's opaque URLs down to the names the doc knows", () => {
    expect(inlineAttachmentNames("![shot](attachment:shot.png) and ![b](attachment:b.png)")).toBe(
      "![shot](shot.png) and ![b](b.png)",
    );
  });

  it("leaves ordinary links untouched", () => {
    const text = "see [the issue](https://example.com/issues/1)";
    expect(inlineAttachmentNames(text)).toBe(text);
  });
});

describe("uniqueName", () => {
  it("keeps both of two screenshots pasted in a row", () => {
    expect(uniqueName("image.png", new Set())).toBe("image.png");
    expect(uniqueName("image.png", new Set(["image.png"]))).toBe("image-2.png");
    expect(uniqueName("image.png", new Set(["image.png", "image-2.png"]))).toBe("image-3.png");
  });

  it("handles a name with no extension", () => {
    expect(uniqueName("dump", new Set(["dump"]))).toBe("dump-2");
  });
});

describe("draftIsEmpty", () => {
  it("is true only for a genuinely untouched form", () => {
    expect(draftIsEmpty(EMPTY_DRAFT)).toBe(true);
    expect(draftIsEmpty({ ...EMPTY_DRAFT, title: "  " })).toBe(true);
    expect(draftIsEmpty({ ...EMPTY_DRAFT, title: "something" })).toBe(false);
    expect(draftIsEmpty({ ...EMPTY_DRAFT, links: [{ url: "https://x" }] })).toBe(false);
  });
});

describe("url helpers", () => {
  it("recognises the issue and PR shapes worth recording as provenance", () => {
    expect(looksLikeIssueUrl("https://github.com/o/r/issues/42")).toBe(true);
    expect(looksLikeIssueUrl("https://github.com/o/r/pull/42")).toBe(true);
    expect(looksLikeIssueUrl("https://gitlab.com/o/r/-/issues/7")).toBe(true);
    expect(looksLikeIssueUrl("https://github.com/o/r")).toBe(false);
  });

  it("de-duplicates urls and drops trailing sentence punctuation", () => {
    expect(extractUrls("see https://a.com/x. and https://a.com/x again")).toEqual([
      "https://a.com/x",
    ]);
  });
});
