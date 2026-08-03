import { useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Check, Copy } from "lucide-react";
import type { Components } from "react-markdown";
import { cn } from "../lib/utils";

/**
 * The react-markdown half of [`Markdown`], split into its own module so `react-markdown` +
 * `remark-gfm` (~200 kB raw, the single heaviest dependency in the bundle) land in a lazily
 * imported chunk instead of the entry. Nothing should import this directly — go through
 * `Markdown`, which owns the prose wrapper and the plain-text fallback shown while this loads.
 */

/** Extract the plain text of a react-markdown child tree, for the code-block copy button. */
function textOf(node: React.ReactNode): string {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(textOf).join("");
  if (typeof node === "object" && "props" in node) {
    return textOf((node as { props: { children?: React.ReactNode } }).props.children);
  }
  return "";
}

/** A fenced code block with a hover-revealed copy button — agent plans are mostly commands and
 * diffs, so copying one out is the single most common thing you do with them. */
function CodeBlock({ children }: { children?: React.ReactNode }) {
  const [copied, setCopied] = useState(false);
  const text = textOf(children);

  const copy = () => {
    const done = () => {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1200);
    };
    if (navigator.clipboard?.writeText) {
      navigator.clipboard.writeText(text).then(done).catch(done);
    } else {
      done();
    }
  };

  return (
    <div className="group/code relative">
      <pre>{children}</pre>
      <button
        type="button"
        onClick={copy}
        aria-label="copy code block"
        title="copy"
        className={cn(
          "absolute right-1.5 top-1.5 inline-flex h-6 items-center gap-1 rounded border border-border bg-card px-1.5 text-[0.68rem] font-medium",
          "opacity-0 transition-opacity focus-visible:opacity-100 group-hover/code:opacity-100",
          copied ? "text-ok" : "text-muted-foreground hover:text-foreground",
        )}
      >
        {copied ? <Check className="h-3 w-3" /> : <Copy className="h-3 w-3" />}
        {copied ? "copied" : "copy"}
      </button>
    </div>
  );
}

/** Links open safely in a new tab; fenced blocks get the copy affordance. */
const BLOCK_COMPONENTS: Components = {
  a: ({ href, children }) => (
    <a href={href} target="_blank" rel="noreferrer noopener">
      {children}
    </a>
  ),
  pre: ({ children }) => <CodeBlock>{children}</CodeBlock>,
};

/** Same, but the block `<p>` is unwrapped so text flows inline inside a list bullet or a table cell. */
const INLINE_COMPONENTS: Components = {
  ...BLOCK_COMPONENTS,
  p: ({ children }) => <>{children}</>,
};

export default function MarkdownBody({ text, inline = false }: { text: string; inline?: boolean }) {
  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm]}
      components={inline ? INLINE_COMPONENTS : BLOCK_COMPONENTS}
    >
      {text}
    </ReactMarkdown>
  );
}
