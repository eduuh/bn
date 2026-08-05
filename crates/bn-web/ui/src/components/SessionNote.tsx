import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { ArrowUpRight, ChevronDown, ExternalLink } from "lucide-react";
import type { Components } from "react-markdown";
import type { AgentDetailView } from "../api/types";
import { codeBlocks, collectReferences, prLink, structuredSections } from "../lib/note";
import { Badge } from "./ui/badge";
import { Card } from "./ui/card";
import { Separator } from "./ui/separator";
import { cn } from "../lib/utils";

/** Shared markdown typography — Tailwind utilities standing in for a prose plugin (preflight is off,
 * so headings/lists/code are styled explicitly here). Reused by the handoff packet render too. */
export const MARKDOWN_PROSE = cn(
  "space-y-3 text-sm leading-relaxed text-foreground/90",
  "[&_h1]:mt-2 [&_h1]:text-lg [&_h1]:font-semibold [&_h1]:tracking-tight [&_h1]:text-foreground",
  "[&_h2]:mt-4 [&_h2]:text-base [&_h2]:font-semibold [&_h2]:text-foreground",
  "[&_h3]:mt-3 [&_h3]:text-sm [&_h3]:font-semibold [&_h3]:text-foreground",
  "[&_a]:font-medium [&_a]:text-primary [&_a]:underline [&_a]:underline-offset-2",
  "[&_ul]:list-disc [&_ul]:pl-5 [&_ol]:list-decimal [&_ol]:pl-5 [&_li]:my-0.5",
  "[&_code]:rounded [&_code]:bg-muted [&_code]:px-1 [&_code]:py-0.5 [&_code]:font-mono [&_code]:text-[0.85em]",
  "[&_pre]:overflow-auto [&_pre]:rounded-md [&_pre]:border [&_pre]:border-border [&_pre]:bg-muted/60 [&_pre]:p-3 [&_pre]:text-xs",
  "[&_pre_code]:bg-transparent [&_pre_code]:p-0",
  "[&_blockquote]:border-l-2 [&_blockquote]:border-border [&_blockquote]:pl-3 [&_blockquote]:text-muted-foreground",
  "[&_table]:w-full [&_table]:text-xs [&_th]:border-b [&_th]:border-border [&_th]:py-1 [&_th]:text-left [&_th]:font-medium",
  "[&_td]:border-b [&_td]:border-border/60 [&_td]:py-1",
);

/** Links in rendered note markdown open safely in a new tab. */
const MD_COMPONENTS: Components = {
  a: ({ href, children }) => (
    <a href={href} target="_blank" rel="noreferrer noopener">
      {children}
    </a>
  ),
};

/** Same, but unwrap the block `<p>` so an item's text renders inline inside its list bullet. */
const INLINE_COMPONENTS: Components = {
  ...MD_COMPONENTS,
  p: ({ children }) => <>{children}</>,
};

function Md({ text, inline = false }: { text: string; inline?: boolean }) {
  return (
    <Markdown remarkPlugins={[remarkGfm]} components={inline ? INLINE_COMPONENTS : MD_COMPONENTS}>
      {text}
    </Markdown>
  );
}

/**
 * The rich session view (#484): the agent's branch note rendered as proper markdown (typography,
 * headings, code blocks), a prominent clickable PR button + linkified resource references, the
 * captured session scripts (dispatch prompt + any command blocks), and the structured note
 * sections (done-when / blockers / open todos as checkboxes, decisions, research, collab, asks).
 * Mobile-first: a single column that reads top-to-bottom on a phone. Pure render of `lib/note`
 * derivations; `null` only when there's neither a note nor a recorded prompt to show.
 */
export function SessionNote({ detail }: { detail: AgentDetailView }) {
  const { note, last_request } = detail;
  const scripts = codeBlocks(note?.body);
  if (!note && !last_request) return null;

  const pr = note ? prLink(note) : null;
  const refs = note ? collectReferences(note) : [];
  const sections = note ? structuredSections(note.front) : [];
  const statusActive = note?.front.status === "active";

  return (
    <Card className="space-y-4 p-4" role="region" aria-labelledby="session-note-title">
      <div className="flex items-center gap-2">
        <h3
          id="session-note-title"
          className="text-sm font-semibold uppercase tracking-wide text-muted-foreground"
        >
          Session note
        </h3>
        {note && <Badge variant={statusActive ? "ok" : "muted"}>{note.front.status}</Badge>}
      </div>

      {(pr || refs.length > 0) && (
        <div className="flex flex-wrap items-center gap-3">
          {pr &&
            (pr.href ? (
              <a
                href={pr.href}
                target="_blank"
                rel="noreferrer noopener"
                className="inline-flex items-center gap-1.5 rounded-md border border-primary/40 bg-primary/10 px-2.5 py-1 text-sm font-medium text-primary transition-colors hover:bg-primary/15"
              >
                <ExternalLink className="h-3.5 w-3.5" />
                PR {pr.label}
                <span className="text-xs text-primary/80">{pr.status}</span>
              </a>
            ) : (
              <span className="inline-flex items-center gap-1.5 rounded-md border border-border bg-muted px-2.5 py-1 text-sm font-medium text-muted-foreground">
                PR {pr.label}
              </span>
            ))}
          {refs.length > 0 && (
            <div className="flex flex-wrap items-center gap-2">
              <span className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
                References
              </span>
              <ul className="flex flex-wrap items-center gap-2" aria-label="resource references">
                {refs.map((r) => (
                  <li key={r.url}>
                    <a
                      href={r.url}
                      target="_blank"
                      rel="noreferrer noopener"
                      className="inline-flex items-center gap-0.5 rounded-md bg-muted px-1.5 py-0.5 text-xs text-primary"
                    >
                      {r.label}
                      <ArrowUpRight className="h-3 w-3" />
                    </a>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </div>
      )}

      {(last_request || scripts.length > 0) && (
        <details className="group rounded-md border border-border bg-muted/30" open>
          <summary className="flex cursor-pointer list-none items-center gap-1.5 px-3 py-2 text-xs font-medium uppercase tracking-wide text-muted-foreground">
            <ChevronDown className="h-3.5 w-3.5 transition-transform group-open:rotate-0 [details:not([open])_&]:-rotate-90" />
            Session scripts
          </summary>
          <div className="space-y-3 px-3 pb-3">
            {last_request && (
              <div className="space-y-1">
                <span className="text-xs font-medium text-muted-foreground">Prompt</span>
                <pre className="overflow-auto rounded-md border border-border bg-muted/60 p-3 font-mono text-xs text-foreground/90">
                  {last_request}
                </pre>
              </div>
            )}
            {scripts.map((s, i) => (
              <div className="space-y-1" key={i}>
                <span className="text-xs font-medium text-muted-foreground">Commands</span>
                <pre className="overflow-auto rounded-md border border-border bg-muted/60 p-3 font-mono text-xs text-foreground/90">
                  {s}
                </pre>
              </div>
            ))}
          </div>
        </details>
      )}

      {note ? (
        <>
          {note.body.trim() && (
            <>
              <Separator />
              <div className={MARKDOWN_PROSE}>
                <Md text={note.body} />
              </div>
            </>
          )}

          {sections.map((s) => (
            <div className="space-y-2" key={s.key}>
              <h4 className="text-sm font-semibold text-foreground">{s.label}</h4>
              <ul
                className={cn(
                  "space-y-1 text-sm",
                  !s.checks && "list-disc pl-5 marker:text-muted-foreground",
                )}
              >
                {s.items.map((it) => (
                  <li
                    key={it.id}
                    className={cn(
                      "flex items-start gap-2",
                      s.key === "blockers" && "text-urgent",
                    )}
                  >
                    {s.checks && (
                      <input
                        type="checkbox"
                        checked={!!it.done}
                        disabled
                        readOnly
                        aria-hidden="true"
                        className="mt-1 h-3.5 w-3.5 shrink-0 accent-primary"
                      />
                    )}
                    <span className="min-w-0 flex-1">
                      <Md text={it.text} inline />
                    </span>
                    {typeof it.pct === "number" && (
                      <span className="shrink-0 text-xs tabular-nums text-muted-foreground">
                        {it.pct}%
                      </span>
                    )}
                    {it.link && /^https?:\/\//.test(it.link) && (
                      <a
                        className="shrink-0 text-primary"
                        href={it.link}
                        target="_blank"
                        rel="noreferrer noopener"
                        aria-label="item link"
                      >
                        <ArrowUpRight className="h-3.5 w-3.5" />
                      </a>
                    )}
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </>
      ) : (
        <p className="text-sm text-muted-foreground">
          No branch note recorded for this session yet.
        </p>
      )}
    </Card>
  );
}
