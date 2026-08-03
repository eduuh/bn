// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import MarkdownBody from "./MarkdownBody";

afterEach(cleanup);

/** The rendered `<pre>`, which is where a fenced block lands. */
function pre(): HTMLElement {
  const el = document.querySelector("pre");
  if (!el) throw new Error("no code block rendered");
  return el as HTMLElement;
}

describe("fenced code blocks", () => {
  it("highlights a block that names a language we ship", () => {
    render(<MarkdownBody text={'```rust\nlet x = "hi";\n```'} />);
    expect(pre().querySelector(".hljs")).not.toBeNull();
    expect(pre().querySelectorAll("[class^=hljs-]").length).toBeGreaterThan(0);
    expect(pre().textContent).toContain('let x = "hi";');
  });

  it("labels the block with its language", () => {
    render(<MarkdownBody text={"```bash\nmake all\n```"} />);
    expect(screen.getByText("bash")).toBeTruthy();
  });

  it("renders an unknown language as plain text, with no label", () => {
    render(<MarkdownBody text={"```brainfuck\n+++.\n```"} />);
    expect(pre().querySelector("[class^=hljs-]")).toBeNull();
    expect(pre().textContent).toContain("+++.");
    // The label still reports what the fence claimed, even though we can't highlight it.
    expect(screen.getByText("brainfuck")).toBeTruthy();
  });

  it("renders a block with no info string as plain text", () => {
    render(<MarkdownBody text={"```\nplain\n```"} />);
    expect(pre().querySelector("[class^=hljs-]")).toBeNull();
    expect(pre().textContent).toContain("plain");
  });

  it("keeps the copy button", () => {
    render(<MarkdownBody text={"```sh\nmake\n```"} />);
    expect(screen.getByLabelText("copy code block")).toBeTruthy();
  });

  it("does not leave a trailing blank line in a highlighted block", () => {
    render(<MarkdownBody text={"```json\n{}\n```"} />);
    expect(pre().textContent).toBe("{}");
  });

  it("escapes markup rather than rendering it, for a block a note body supplied", () => {
    render(<MarkdownBody text={'```rust\nlet s = "<img src=x onerror=alert(1)>";\n```'} />);
    expect(pre().querySelector("img")).toBeNull();
    expect(pre().textContent).toContain("<img src=x onerror=alert(1)>");
  });
});

describe("headings", () => {
  it("gets a slug id and a self link", () => {
    render(<MarkdownBody text={"## Done when\n\nbody"} />);
    const h = document.querySelector("h2");
    expect(h?.id).toBe("done-when");
    expect(h?.querySelector('a[href="#done-when"]')).not.toBeNull();
  });

  it("strips punctuation and collapses whitespace in the slug", () => {
    render(<MarkdownBody text={"### Step 1: fix the *parser*!\n"} />);
    expect(document.querySelector("h3")?.id).toBe("step-1-fix-the-parser");
  });

  it("leaves the id off a heading that slugs to nothing", () => {
    render(<MarkdownBody text={"## !!!\n"} />);
    const h = document.querySelector("h2");
    expect(h?.id).toBe("");
    expect(h?.querySelector("a")).toBeNull();
  });
});

describe("other block content", () => {
  it("constrains images and gives them alt text", () => {
    render(<MarkdownBody text={"![a chart](/img/chart.png)"} />);
    const img = screen.getByAltText("a chart") as HTMLImageElement;
    expect(img.className).toContain("max-w-full");
    expect(img.getAttribute("loading")).toBe("lazy");
  });

  it("opens links in a new tab, safely", () => {
    render(<MarkdownBody text={"[docs](https://example.com)"} />);
    const a = screen.getByText("docs") as HTMLAnchorElement;
    expect(a.target).toBe("_blank");
    expect(a.rel).toContain("noopener");
  });

  it("still renders GFM tables and task lists", () => {
    render(<MarkdownBody text={"| a | b |\n| - | - |\n| 1 | 2 |\n\n- [x] done\n- [ ] todo"} />);
    expect(document.querySelector("table")).not.toBeNull();
    expect(document.querySelectorAll('input[type="checkbox"]').length).toBe(2);
  });

  it("does not render raw HTML from a note body", () => {
    render(<MarkdownBody text={'<img src=x onerror="alert(1)">\n\nafter'} />);
    expect(document.querySelector("img")).toBeNull();
  });

  it("unwraps the paragraph in inline mode so text flows in a bullet", () => {
    const { container } = render(<MarkdownBody text="just text" inline />);
    expect(container.querySelector("p")).toBeNull();
    expect(container.textContent).toBe("just text");
  });
});
