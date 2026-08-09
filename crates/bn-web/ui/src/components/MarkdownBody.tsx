import { useMemo, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Check, Copy } from "lucide-react";
import type { Components } from "react-markdown";
import { cn } from "../lib/utils";
import { highlightCode, languageOf } from "../lib/highlight";

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

/** The props of the `<code>` element react-markdown nests inside a fenced block's `<pre>` — where
 * the fence's info string lives, as a `language-…` class. */
function codeChildProps(
  children: React.ReactNode,
): { className?: string; children?: React.ReactNode } | null {
  // `Array.isArray` narrows ReactNode to `any[]`, so index it through an explicit element type
  // rather than letting `only` come out as `any`.
  const only: React.ReactNode = Array.isArray(children)
    ? (children as React.ReactNode[])[0]
    : children;
  if (only && typeof only === "object" && "props" in only) {
    return (only as { props: { className?: string; children?: React.ReactNode } }).props;
  }
  return null;
}

/** A fenced code block with a hover-revealed copy button — agent plans are mostly commands and
 * diffs, so copying one out is the single most common thing you do with them. Blocks that name a
 * language we ship a grammar for are syntax-highlighted; the rest render as plain text. */
function CodeBlock({ children }: { children?: React.ReactNode }) {
  const [copied, setCopied] = useState(false);
  const text = textOf(children);
  const language = languageOf(codeChildProps(children)?.className);
  // react-markdown keeps the fence's trailing newline; highlighting it would add a blank last line.
  const html = useMemo(() => highlightCode(text.replace(/\n$/, ""), language), [text, language]);

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
      <pre>
        {html === null ? (
          children
        ) : (
          // Safe: highlight.js escapes the source it is handed, so no markup survives from the
          // note body — only the `<span class="hljs-…">` wrappers it adds itself.
          <code className="hljs" dangerouslySetInnerHTML={{ __html: html }} />
        )}
      </pre>
      {/* Right-aligned row: the language reads as a quiet label, the copy button fades in on
          hover. Both keep their box at all times, so nothing shifts as it appears. */}
      <div className="absolute right-1.5 top-1.5 flex items-center gap-1.5">
        {language && (
          <span className="select-none font-mono text-[0.65rem] uppercase tracking-wide text-muted-foreground/70">
            {language}
          </span>
        )}
        <button
          type="button"
          onClick={copy}
          aria-label="copy code block"
          title="copy"
          className={cn(
            "inline-flex h-6 items-center gap-1 rounded border border-border bg-card px-1.5 text-[0.68rem] font-medium",
            "opacity-0 transition-opacity focus-visible:opacity-100 group-hover/code:opacity-100",
            copied ? "text-ok" : "text-muted-foreground hover:text-foreground",
          )}
        >
          {copied ? <Check className="h-3 w-3" /> : <Copy className="h-3 w-3" />}
          {copied ? "copied" : "copy"}
        </button>
      </div>
    </div>
  );
}

/** A GitHub-style slug for a heading, so a long plan or handoff packet can be linked section-wise. */
function slugify(text: string): string {
  return text
    .toLowerCase()
    .trim()
    .replace(/[^\w\s-]/g, "")
    .replace(/\s+/g, "-")
    .replace(/-+/g, "-");
}

/** Renders one heading level with a hover-revealed `#` anchor pointing at its own slug. */
function heading(Tag: "h1" | "h2" | "h3" | "h4") {
  return function Heading({ children }: { children?: React.ReactNode }) {
    const id = slugify(textOf(children));
    return (
      <Tag id={id || undefined} className="group/h scroll-mt-16">
        {children}
        {id && (
          <a
            href={`#${id}`}
            aria-label="link to this section"
            className="ml-1.5 select-none font-normal text-muted-foreground no-underline opacity-0 transition-opacity hover:opacity-100 focus-visible:opacity-100 group-hover/h:opacity-60"
          >
            #
          </a>
        )}
      </Tag>
    );
  };
}

/**
 * Turn a markdown image source into something the browser can actually fetch.
 *
 * A task doc refers to its attachments by bare name (`![shot.png](shot.png)`) because a name is the
 * only identifier that survives the doc syncing to another machine and moving on pickup — see
 * `bn_core::task_attach`. That is right for the document and useless to an `<img>`, which would
 * resolve it against the current page. A document that knows where its assets are served from
 * passes a resolver; everything else renders sources verbatim.
 */
export type AssetResolver = (src: string) => string;

/** Links open safely in a new tab; fenced blocks get highlighting and the copy affordance;
 * headings get linkable slugs; images are constrained so a wide asset can't blow out the column. */
function blockComponents(resolveAsset?: AssetResolver): Components {
  return {
    a: ({ href, children }) => (
      <a href={href} target="_blank" rel="noreferrer noopener">
        {children}
      </a>
    ),
    pre: ({ children }) => <CodeBlock>{children}</CodeBlock>,
    h1: heading("h1"),
    h2: heading("h2"),
    h3: heading("h3"),
    h4: heading("h4"),
    img: ({ src, alt, title }) => {
      const raw = typeof src === "string" ? src : undefined;
      return (
        <img
          src={raw && resolveAsset ? resolveAsset(raw) : raw}
          alt={alt ?? ""}
          title={title}
          loading="lazy"
          className="my-2 h-auto max-w-full rounded-md border border-border"
        />
      );
    },
  };
}

const BLOCK_COMPONENTS: Components = blockComponents();

/** Same, but the block `<p>` is unwrapped so text flows inline inside a list bullet or a table cell. */
const INLINE_COMPONENTS: Components = {
  ...BLOCK_COMPONENTS,
  p: ({ children }) => <>{children}</>,
};

export default function MarkdownBody({
  text,
  inline = false,
  resolveAsset,
}: {
  text: string;
  inline?: boolean;
  resolveAsset?: AssetResolver;
}) {
  const components = useMemo(() => {
    if (!resolveAsset) return inline ? INLINE_COMPONENTS : BLOCK_COMPONENTS;
    const block = blockComponents(resolveAsset);
    return inline
      ? { ...block, p: ({ children }: { children?: React.ReactNode }) => <>{children}</> }
      : block;
  }, [inline, resolveAsset]);

  return (
    <ReactMarkdown remarkPlugins={[remarkGfm]} components={components}>
      {text}
    </ReactMarkdown>
  );
}
