import { AlertTriangle, CircleDot, Hammer, Layers, Timer } from "lucide-react";

import type { QueueMachine } from "../api/queue-types";
import { Card } from "./ui/card";
import { cn } from "../lib/utils";

/**
 * The state of the machine, above everything else on the page.
 *
 * `/queue` exists to answer "why is nothing running", and it used to open with `1 pending · 1 node`
 * — two numbers that cannot answer it. The four facts that can were computed for `bn queue status`
 * and shipped nowhere else, so the terminal could explain a stalled queue and the dashboard could
 * only show you that it was stalled.
 *
 * Ordered by what invalidates what. A dead dispatcher makes every number under it meaningless —
 * work is being launched directly, outside the caps — so that reads first and loudly. Then the
 * lanes and the build pool, which are the usual reason something is waiting. A full lane is the
 * single most common answer, so it is coloured rather than left for the reader to compare two
 * numerals.
 */
function Stat({
  icon,
  label,
  value,
  tone = "muted",
  title,
}: {
  icon: React.ReactNode;
  label: string;
  value: string;
  tone?: "muted" | "ok" | "warn" | "bad";
  title?: string;
}) {
  return (
    <div className="flex items-center gap-2" title={title}>
      <span
        className={cn(
          "shrink-0",
          tone === "bad" && "text-destructive",
          tone === "warn" && "text-warn",
          tone === "ok" && "text-ok",
          tone === "muted" && "text-muted-foreground",
        )}
      >
        {icon}
      </span>
      <span className="text-xs text-muted-foreground">{label}</span>
      <span
        className={cn(
          "font-mono text-sm tabular-nums",
          tone === "bad" && "text-destructive",
          tone === "warn" && "text-warn",
        )}
      >
        {value}
      </span>
    </div>
  );
}

export function QueueStatus({ machine, pending }: { machine?: QueueMachine; pending: number }) {
  if (!machine) return null;
  const { dispatcher_alive: alive, lanes, builds, windows } = machine;

  return (
    <Card className={cn("p-3", !alive && "border-destructive/50 bg-destructive/5")}>
      {!alive ? (
        <p className="mb-2 flex items-center gap-2 text-sm text-destructive">
          <AlertTriangle className="size-4 shrink-0" />
          <span>
            <strong>No dispatcher is running.</strong> Nothing here will start on its own, and
            {machine.captured.length > 0
              ? ` ${machine.captured.join(", ")} are launching directly — outside the caps below.`
              : " capture is off."}
          </span>
        </p>
      ) : null}

      <div className="flex flex-wrap items-center gap-x-6 gap-y-2">
        <Stat
          icon={<CircleDot className={cn("size-3.5", alive && "animate-pulse")} />}
          label="dispatcher"
          value={alive ? "live" : "down"}
          tone={alive ? "ok" : "bad"}
          title={alive ? `capturing ${machine.captured.join(", ")}` : "no tick in the last 10 min"}
        />
        {lanes.map((l) => {
          const full = l.cap > 0 && l.used >= l.cap;
          return (
            <Stat
              key={l.lane}
              icon={<Layers className="size-3.5" />}
              label={l.lane}
              value={`${l.used}/${l.cap}`}
              // A full lane is the commonest reason work is waiting; say so in colour rather than
              // leaving two numerals to be compared.
              tone={full ? "warn" : "muted"}
              title={full ? `${l.lane} lane is full — ${machine.lanes_why}` : machine.lanes_why}
            />
          );
        })}
        <Stat
          icon={<Hammer className="size-3.5" />}
          label="builds"
          value={`${builds.held.length}/${builds.cap}`}
          tone={builds.held.length >= builds.cap ? "warn" : "muted"}
          title={
            builds.held.length > 0
              ? builds.held.map((h) => `${h.repo}/${h.label}`).join(", ")
              : "no verify gate running"
          }
        />
        <Stat icon={<Timer className="size-3.5" />} label="pending" value={String(pending)} />
        {windows.map((w) => (
          <Stat
            key={w.name}
            icon={<Timer className="size-3.5" />}
            label={`window ${w.name}`}
            value={`${w.boost > 0 ? "+" : ""}${w.boost}`}
            tone="ok"
            title={`boosting ${w.matches.join(", ")}`}
          />
        ))}
      </div>
    </Card>
  );
}
