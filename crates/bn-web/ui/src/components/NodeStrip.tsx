import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";

import { nodesQuery } from "../api/client";
import { LEVEL_LABEL, LEVEL_TONE } from "./MachineHealth";
import { cn } from "../lib/utils";

/**
 * The machines behind the fleet, as a strip of chips above the agent cards.
 *
 * The clay prototype draws this strip as a *filter* — click a node, see only its agents. That is
 * the right end state, but nothing in the API carries it yet: neither `FleetEntry` (`lib/fleet.ts`)
 * nor `AgentSession` records which machine an agent runs on, so a filter would either be a no-op or
 * a lie. Until agents report node identity this is the honest half of that design — which machines
 * exist, whether they answer, and how loaded each one is — and each chip jumps to its full card on
 * `/nodes`.
 */
export function NodeStrip() {
  const q = useQuery(nodesQuery());
  const nodes = q.data?.nodes ?? [];

  if (nodes.length === 0) return null;

  return (
    <div
      className="grid gap-2.5 [grid-template-columns:repeat(auto-fit,minmax(230px,1fr))]"
      aria-label="fleet nodes"
    >
      {nodes.map((n) => {
        const down = n.status === "unreachable";
        const tone = LEVEL_TONE[n.saturation.level];
        return (
          <Link
            key={n.host}
            to="/nodes"
            className={cn(
              "flex items-center gap-2.5 rounded-[10px] border border-border bg-card px-3 py-2.5 no-underline transition-colors hover:border-input",
              down && "border-urgent/40",
            )}
            title={
              down ? `${n.host} — unreachable` : `${n.host} — ${LEVEL_LABEL[n.saturation.level]}`
            }
          >
            <span
              className={cn(
                "h-[7px] w-[7px] flex-none rounded-full",
                down ? "bg-urgent" : cn(tone.bar, "animate-pulse"),
              )}
              aria-hidden="true"
            />
            <span className="truncate font-mono text-xs font-semibold text-foreground">
              {n.host}
            </span>
            <span
              className={cn(
                "ml-auto flex-none font-mono text-[10.5px] tabular-nums",
                down ? "text-urgent" : "text-muted-foreground/70",
              )}
            >
              {down
                ? "unreachable"
                : `${n.agents.active} agent${n.agents.active === 1 ? "" : "s"}`}
            </span>
          </Link>
        );
      })}
    </div>
  );
}
