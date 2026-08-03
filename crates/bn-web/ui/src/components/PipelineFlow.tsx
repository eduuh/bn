import type { PipelineStage } from "../api/types";
import { cn } from "../lib/utils";

/**
 * The nine-stage pipeline, drawn (#1006).
 *
 * EPIC #746 built `pickup → investigate → plan → consensus → execute → validate → critique → ship
 * → learn` as a durable state machine, and until #1005 the dashboard could not see a single stage
 * of it. This is that machine as a picture: one node per stage, carrying how many worktrees sit
 * there right now.
 *
 * **Inline SVG, no chart library.** The dashboard is embedded in the bn binary and served over a
 * LAN that often has no route to the internet, so a CDN import would silently fail — the same
 * reason the fonts are self-hosted. It is also nine rectangles; a library would be more code than
 * the drawing.
 *
 * Clicking a stage filters the task list beside it, so the drawing is a control rather than
 * decoration. An empty stage stays visible and dim: knowing that nothing is in `consensus` is a
 * fact about the pipeline, and hiding it would make the flow read as shorter than it is.
 */
export function PipelineFlow({
  stages,
  selected,
  onSelect,
}: {
  stages: PipelineStage[];
  selected?: string;
  onSelect?: (stage: string | undefined) => void;
}) {
  if (stages.length === 0) return null;
  const total = stages.reduce((n, s) => n + s.count, 0);

  return (
    <section aria-label="pipeline" className="space-y-1.5">
      <div className="flex items-baseline gap-2">
        <h2 className="font-display text-sm font-semibold tracking-tight text-foreground">
          Pipeline
        </h2>
        <span className="font-mono text-[11px] tabular-nums text-muted-foreground/70">
          {total === 0 ? "nothing in flight" : `${total} in flight`}
        </span>
        {selected && (
          <button
            type="button"
            onClick={() => onSelect?.(undefined)}
            className="ml-auto text-[11px] text-primary hover:underline"
          >
            clear filter
          </button>
        )}
      </div>

      {/* Horizontal scroll rather than a wrap: the flow is an *order*, and wrapping it mid-way
          reads as two pipelines. The container scrolls; the page never does. */}
      <div className="overflow-x-auto pb-1">
        <ol className="flex min-w-max items-stretch gap-0">
          {stages.map((stage, i) => {
            const active = selected === stage.id;
            const occupied = stage.count > 0;
            return (
              <li key={stage.id} className="flex items-stretch">
                <button
                  type="button"
                  aria-pressed={active}
                  aria-label={`${stage.id}: ${stage.count} in flight`}
                  onClick={() => onSelect?.(active ? undefined : stage.id)}
                  className={cn(
                    "flex min-w-[6.6rem] max-w-[8rem] flex-col items-center gap-0.5 rounded-[9px] border px-2 py-1.5 transition-colors",
                    active
                      ? "border-primary bg-primary/[0.13] text-primary"
                      : occupied
                        ? "border-border bg-card text-foreground hover:border-primary/50"
                        : "border-border/60 bg-transparent text-muted-foreground/60 hover:border-border",
                  )}
                >
                  <span aria-hidden="true" className="text-base leading-none">
                    {stage.glyph}
                  </span>
                  <span className="text-[10.5px] font-medium leading-none">{stage.id}</span>
                  <span
                    className={cn(
                      "font-mono text-[11px] leading-none tabular-nums",
                      occupied ? "font-semibold" : "opacity-60",
                    )}
                  >
                    {stage.count}
                  </span>
                  {/* The skill this stage dispatches — the node then says what the stage *does*,
                      not only how many sit in it. A stage that runs no workflow (pickup leases,
                      consensus votes, validate is `bn verify`) shows nothing rather than a
                      plausible-looking placeholder. */}
                  {stage.skill && (
                    <span
                      title={`runs the \`${stage.skill}\` skill`}
                      className="w-full truncate font-mono text-[9.5px] leading-none text-muted-foreground/70"
                    >
                      {stage.skill}
                    </span>
                  )}
                </button>
                {i < stages.length - 1 && (
                  <span
                    aria-hidden="true"
                    className="self-center px-0.5 text-muted-foreground/40"
                  >
                    →
                  </span>
                )}
              </li>
            );
          })}
        </ol>
      </div>
    </section>
  );
}
