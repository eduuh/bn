import { lazy, Suspense } from "react";
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

// react-markdown + remark-gfm are the bundle's heaviest dependency and nothing above the fold needs
// them synchronously, so the renderer is a lazy chunk. Until it lands, the raw source is shown —
// markdown degrades to legible plain text, so the fallback is the content rather than a spinner.
const MarkdownBody = lazy(() => import("./MarkdownBody"));

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
    <Suspense fallback={<span className="whitespace-pre-wrap">{text}</span>}>
      <MarkdownBody text={text} inline={inline} />
    </Suspense>
  );
  if (inline || bare) return className ? <span className={className}>{body}</span> : body;
  return <div className={cn(MARKDOWN_PROSE, className)}>{body}</div>;
}
