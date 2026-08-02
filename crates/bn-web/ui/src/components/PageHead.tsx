import { cn } from "../lib/utils";

/**
 * The one page heading every non-console route wears: a display-serif title with a mono sub-line
 * beside it, and an optional slot on the right for the page's own controls.
 *
 * The routes used to each hand-roll `<h2 className="text-2xl font-semibold tracking-tight">`, which
 * is why the serif — the face that makes the surface read as Claude rather than as a generic
 * dashboard — appeared on exactly one screen. Centralising the head means the type ramp is now a
 * property of the system instead of a thing twelve files happen to agree on.
 */
export function PageHead({
  title,
  sub,
  right,
  className,
}: {
  title: React.ReactNode;
  sub?: React.ReactNode;
  right?: React.ReactNode;
  className?: string;
}) {
  return (
    <div className={cn("mb-4 flex flex-wrap items-baseline gap-x-3 gap-y-1", className)}>
      <h2 className="font-display text-2xl font-semibold tracking-[-0.015em] text-foreground">
        {title}
      </h2>
      {sub && (
        <span className="font-mono text-[11.5px] tabular-nums text-muted-foreground/70">{sub}</span>
      )}
      {right && <div className="ml-auto flex items-center gap-2">{right}</div>}
    </div>
  );
}
