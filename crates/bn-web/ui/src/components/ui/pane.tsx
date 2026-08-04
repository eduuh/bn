import * as React from "react";

import { cn } from "@/lib/utils";

/**
 * The console language, extracted.
 *
 * `/console` was the one screen in the app that read as a tool rather than a template, and the
 * reason is structural: it is built from panes — a flush header on one baseline with its counts
 * pushed to the right edge, and a body that scrolls *inside* the pane instead of scrolling the
 * whole page. Everything else was built from `<Card>`, which has a border, a radius, a shadow and
 * page-level scrolling, so a page section, a list row, a stat tile and a form panel all rendered as
 * the same floating rectangle. 113 of them, no hierarchy.
 *
 * `Pane` is the shape `/console` already used, made reusable so the other screens can speak it.
 * Panes tile edge-to-edge and are divided by rules rather than gaps: in a dense grid a 1px shared
 * border reads as structure, where an 8px gap between two rounded cards reads as debris.
 */
export function Pane({
  title,
  meta,
  actions,
  children,
  className,
  bodyClassName,
  scroll = true,
}: {
  title?: React.ReactNode;
  /** Status line for the right edge of the header — counts, freshness, a state word. */
  meta?: React.ReactNode;
  actions?: React.ReactNode;
  children: React.ReactNode;
  className?: string;
  bodyClassName?: string;
  /** Panes scroll their own body by default; set false to let the pane grow with its content. */
  scroll?: boolean;
}) {
  return (
    <section className={cn("flex min-h-0 flex-col bg-card", className)}>
      {(title || meta || actions) && (
        // `flex-wrap`: a pane in a 22rem side column cannot fit a title, a status line and a button
        // on one baseline, and without wrapping the button simply clipped off the pane's edge.
        <header className="flex flex-none flex-wrap items-baseline gap-x-2 gap-y-1.5 border-b border-border px-3.5 py-2.5">
          {title && (
            // `shrink-0`: a busy status line wraps, and without it the flex row squeezes the title
            // until it breaks mid-word.
            <h2 className="shrink-0 font-display text-sm font-semibold tracking-tight text-foreground">
              {title}
            </h2>
          )}
          {meta && (
            <span className="min-w-0 truncate font-mono text-[11px] tabular-nums text-muted-foreground/70">
              {meta}
            </span>
          )}
          {actions && <div className="ml-auto flex shrink-0 items-center gap-1.5">{actions}</div>}
        </header>
      )}
      <div className={cn("min-h-0 flex-1", scroll && "overflow-y-auto", bodyClassName)}>
        {children}
      </div>
    </section>
  );
}

/**
 * A labelled region *inside* a pane — the thing most of the old `<Card>`s were actually being used
 * for. No border, no shadow, no radius: a small-caps label and the content under it. Hierarchy
 * comes from the label and the spacing, which is what lets the pane's own border be the only edge
 * on screen and keeps a dense column readable.
 */
export function Section({
  label,
  meta,
  actions,
  children,
  className,
}: {
  label?: React.ReactNode;
  meta?: React.ReactNode;
  actions?: React.ReactNode;
  children: React.ReactNode;
  className?: string;
}) {
  // A section with a plain-text label is a *landmark*: `<section>` takes `role="region"` once it
  // has an accessible name, which is what lets a screen reader jump between "Needs you", "Working"
  // and the rest instead of walking the whole board. The routes this replaced hand-rolled
  // `<section aria-label=…>` for exactly that, so rendering a bare `<div>` here would have quietly
  // dropped every landmark on the page.
  const Tag = typeof label === "string" ? "section" : "div";
  return (
    <Tag
      aria-label={typeof label === "string" ? label : undefined}
      className={cn("px-3.5 py-3", className)}
    >
      {(label || meta || actions) && (
        <div className="mb-2 flex items-baseline gap-2">
          {label && (
            <h3 className="shrink-0 text-[0.68rem] font-semibold uppercase tracking-[0.08em] text-muted-foreground">
              {label}
            </h3>
          )}
          {meta && (
            <span className="truncate font-mono text-[11px] tabular-nums text-muted-foreground/60">
              {meta}
            </span>
          )}
          {actions && <div className="ml-auto flex shrink-0 items-center gap-1.5">{actions}</div>}
        </div>
      )}
      {children}
    </Tag>
  );
}

/** A hairline divider between stacked sections in one pane — the rule that replaces a card gap. */
export function PaneRule({ className }: { className?: string }) {
  return <div className={cn("h-px flex-none bg-border", className)} aria-hidden="true" />;
}
