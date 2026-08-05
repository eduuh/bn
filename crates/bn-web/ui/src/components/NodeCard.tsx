import { AlertTriangle, Cpu, HardDrive, MemoryStick } from "lucide-react";

import type { NodeView } from "../api/types";
import { Card } from "./ui/card";
import { Gauge } from "./MachineHealth";
import { gaugeTone, gb, LEVEL_LABEL, LEVEL_TONE } from "../lib/machine";
import { cn } from "../lib/utils";

/** Human platform label — `std::env::consts::OS` values the fleet can actually run on. */
const PLATFORM_LABEL: Record<string, string> = {
  macos: "darwin",
  linux: "linux",
  windows: "windows",
};

/**
 * One fleet machine as a card (#966 nodes view): identity (host · platform · arch), a
 * reachability pill, the machine gauges (CPU / RAM / swap), and how many agents load it.
 * An `unreachable` node drops its gauges — stale numbers would be worse than none — and
 * says so in the signal scale instead.
 */
export function NodeCard({ node }: { node: NodeView }) {
  const unreachable = node.status === "unreachable";
  const level = node.saturation.level;
  const tone = LEVEL_TONE[level];
  const loadTone = gaugeTone(node.cpu.load_per_core, 0.7, 1.0);
  const ramTone = gaugeTone(node.ram.used_frac, 0.85, 0.95);
  const swapTone = gaugeTone(node.ram.swap_used_frac, 0.1, 0.5);
  const load1 = node.cpu.load1 == null ? "—" : node.cpu.load1.toFixed(2);

  return (
    <Card
      className={cn("space-y-3 p-3", unreachable && "border-urgent/40")}
      aria-label={`node ${node.host}`}
    >
      <div className="flex items-center gap-2">
        <span className="truncate font-mono text-sm font-semibold text-foreground">
          {node.host}
        </span>
        {unreachable ? (
          <span className="inline-flex items-center gap-1 rounded-full border border-urgent/30 bg-urgent-muted px-2 py-0.5 text-xs font-medium text-urgent">
            <AlertTriangle className="h-3 w-3" />
            unreachable
          </span>
        ) : (
          <span
            className={cn(
              "inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-xs font-medium",
              tone.badge,
            )}
          >
            {LEVEL_LABEL[level]}
          </span>
        )}
        {node.local && (
          <span className="inline-flex items-center rounded-full border border-info/30 bg-info-muted px-2 py-0.5 text-xs font-medium text-info">
            local
          </span>
        )}
        <span className="ml-auto flex-none font-mono text-xs text-muted-foreground/70">
          {PLATFORM_LABEL[node.platform] ?? node.platform} · {node.arch}
        </span>
      </div>

      {unreachable ? (
        <div className="rounded-md border border-urgent/30 bg-urgent-muted px-2 py-1.5 font-mono text-xs text-urgent">
          no heartbeat — vitals unavailable
        </div>
      ) : (
        <div className="space-y-2.5">
          <Gauge
            label={
              <span className="inline-flex items-center gap-1">
                <Cpu className="h-3 w-3" /> CPU load
              </span>
            }
            frac={node.cpu.load_per_core}
            detail={`${load1} / ${node.cpu.cores} cores`}
            tone={loadTone}
          />
          <Gauge
            label={
              <span className="inline-flex items-center gap-1">
                <MemoryStick className="h-3 w-3" /> RAM
              </span>
            }
            frac={node.ram.used_frac}
            detail={`${gb(node.ram.used_mb)} / ${gb(node.ram.total_mb)}`}
            tone={ramTone}
          />
          <Gauge
            label={
              <span className="inline-flex items-center gap-1">
                <HardDrive className="h-3 w-3" /> Swap
              </span>
            }
            frac={node.ram.swap_used_frac}
            detail={
              node.ram.swap_total_mb === 0
                ? "none"
                : `${gb(node.ram.swap_used_mb)} / ${gb(node.ram.swap_total_mb)}`
            }
            tone={swapTone}
          />
        </div>
      )}

      <div className="flex items-center justify-between text-xs text-muted-foreground">
        <span>
          {node.agents.active === 0
            ? "no agents — accepts dispatch"
            : `${node.agents.active} agent${node.agents.active === 1 ? "" : "s"} loading it`}
        </span>
      </div>

      {!unreachable && node.saturation.reasons.length > 0 && (
        <div className={cn("rounded-md border px-2 py-1.5 text-xs", tone.badge)}>
          {node.saturation.reasons.join(" · ")}
        </div>
      )}
    </Card>
  );
}
