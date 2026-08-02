import { useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Check, Copy } from "lucide-react";
import type { Components } from "react-markdown";
import { cn } from "../lib/utils";

/**
 * Shared markdown typography — Tailwind utilities standing in for a prose plugin (preflight is off,
 * so headings/lists/code/tables are styled explicitly here). This is the dashboard's single
 * markdown look: branch notes, plan bodies, handoff packets, skills, research and action details
 * all render through [`Markdown`] so they read the same everywhere.
 */
export const MARKDOWN_PROSE = cn(
  "space-y-3 text-sm leading-relaxed text-foreground/90",
  "[&_h1]:mt-2 [&_h1]:text-lg [&_h1]:font-semibold [&_h1]:tracking-tight [&_h1]:text-foreground",
  "[&_h2]:mt-4 [&_h2]:text-base [&_h2]:font-semibold [&_h2]:text-foreground",
  "[&_h3]:mt-3 [&_h3]:text-sm [&_h3]:font-semibold [&_h3]:text-foreground",
  "[&_h4]:mt-3 [&_h4]:text-sm [&_h4]:font-semibold [&_h4]:text-muted-foreground",
  "[&_a]:font-medium [&_a]:text-primary [&_a]:underline [&_a]:underline-offset-2",
  "[&_ul]:list-disc [&_ul]:pl-5 [&_ol]:list-decimal [&_ol]:pl-5 [&_li]:my-0.5",
  "[&_code]:rounded [&_code]:bg-muted [&_code]:px-1 [&_code]:py-0.5 [&_code]:font-mono [&_code]:text-[0.85em]",
  "[&_pre]:overflow-auto [&_pre]:rounded-md [&_pre]:border [&_pre]:border-border [&_pre]:bg-muted/60 [&_pre]:p-3 [&_pre]:text-xs",
  "[&_pre_code]:bg-transparent [&_pre_code]:p-0",
  "[&_blockquote]:border-l-2 [&_blockquote]:border-border [&_blockquote]:pl-3 [&_blockquote]:text-muted-foreground",
  "[&_hr]:border-border",
  "[&_table]:w-full [&_table]:text-xs [&_th]:border-b [&_th]:border-border [&_th]:py-1 [&_th]:text-left [&_th]:font-medium",
  "[&_td]:border-b [&_td]:border-border/60 [&_td]:py-1",
  // GFM task lists: the generated checkbox is decorative here (notes are read-only in the web).
  "[&_li:has(>input[type=checkbox])]:list-none [&_li:has(>input[type=checkbox])]:-ml-5",
  "[&_input[type=checkbox]]:mr-1.5 [&_input[type=checkbox]]:accent-primary",
);

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

/**
 * The dashboard's markdown renderer. GFM (tables, task lists, strikethrough, autolinks) is on;
 * raw HTML is *not* rendered (react-markdown skips it by default), so note bodies written by agents
 * can't inject markup.
 *
 * - `inline` unwraps paragraphs, for rendering a single list item's text.
 * - `className` composes onto the prose wrapper; pass `MARKDOWN_PROSE` yourself only if you need
 *   the styles on an outer element instead (as the handoff packet does).
 * - `bare` skips the prose wrapper entirely when an ancestor already applies it.
 */
export function Markdown({
  text,
  inline = false,
  bare = false,
  className,
}: {
  text: string;
  inline?: boolean;
  bare?: boolean;
  className?: string;
}) {
  const body = (
    <ReactMarkdown
      remarkPlugins={[remarkGfm]}
      components={inline ? INLINE_COMPONENTS : BLOCK_COMPONENTS}
    >
      {text}
    </ReactMarkdown>
  );
  if (inline || bare) return className ? <span className={className}>{body}</span> : body;
  return <div className={cn(MARKDOWN_PROSE, className)}>{body}</div>;
}
