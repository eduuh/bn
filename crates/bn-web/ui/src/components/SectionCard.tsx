import type { ReactNode } from "react";

import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./ui/card";
import { Skeleton } from "./ui/skeleton";
import { Tooltip, TooltipContent, TooltipTrigger } from "./ui/tooltip";
import { cn } from "../lib/utils";

/**
 * One band of a card-language screen — the shared shape behind Home's deck and Today's digest.
 *
 * It is built from the real `Card` parts rather than a stack of styled `div`s: `CardHeader` /
 * `CardTitle` / `CardDescription` mean the count reads as the description *of* the title it belongs
 * to, so the heading is structured content instead of three spans that happen to line up.
 *
 * The eyebrow is deliberately not a `CardTitle` of its own. It is a category ("Start now",
 * "Autonomous"), not a name, and promoting it to a heading would put a second entry in the
 * document outline for every section on the page.
 */
export function SectionCard({
  eyebrow,
  title,
  count,
  action,
  tone,
  className,
  children,
}: {
  eyebrow?: string;
  title: ReactNode;
  /** The status line beside the title — a count, a ratio, a freshness stamp. */
  count?: ReactNode;
  action?: ReactNode;
  /** `ready` tints the whole card in the ok tone — the one band you are meant to act on. */
  tone?: "ready";
  className?: string;
  children: ReactNode;
}) {
  return (
    <Card className={cn(tone === "ready" && "border-ok/40 bg-ok/[0.05]", className)}>
      <CardHeader className="flex-row flex-wrap items-baseline gap-x-2.5 gap-y-1 space-y-0 p-3.5 pb-2.5">
        {eyebrow && (
          <span className="font-mono text-[10px] font-medium uppercase tracking-[0.14em] text-muted-foreground/70">
            {eyebrow}
          </span>
        )}
        <CardTitle className="text-sm">{title}</CardTitle>
        {count && (
          <CardDescription className="font-mono text-[11px] tabular-nums text-muted-foreground/70">
            {count}
          </CardDescription>
        )}
        {action && <span className="ml-auto self-center">{action}</span>}
      </CardHeader>
      <CardContent className="p-3.5 pt-0">{children}</CardContent>
    </Card>
  );
}

/** Placeholder rows for a section whose query has not resolved. An explicit skeleton beats a
 * "loading…" caption over an empty body, which reads as *"nothing here"* for the first second. */
export function RowsSkeleton({ rows = 3, className }: { rows?: number; className?: string }) {
  return (
    <div className="space-y-2">
      {Array.from({ length: rows }, (_, i) => (
        <Skeleton key={i} className={cn("h-[46px] w-full", className)} />
      ))}
    </div>
  );
}

/** A section with nothing in it — said in a sentence that names the way out. */
export function Empty({ children }: { children: ReactNode }) {
  return <p className="py-2 text-sm text-muted-foreground">{children}</p>;
}

/** A failed query, said in the urgent tone. An empty list and a failed fetch are different facts
 * and must never render alike. */
export function LoadError({ children }: { children: ReactNode }) {
  return <p className="py-2 text-sm text-urgent">{children}</p>;
}

/** A short explanatory tooltip. `asChild` keeps the trigger's own element — no extra wrapper in the
 * layout — which matters because most of these sit inside a `flex-wrap` row. Requires a
 * `TooltipProvider` above it; each page mounts one at its root. */
export function Hint({ label, children }: { label: ReactNode; children: ReactNode }) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>{children}</TooltipTrigger>
      <TooltipContent className="max-w-xs">{label}</TooltipContent>
    </Tooltip>
  );
}
