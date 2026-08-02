import { useQuery } from "@tanstack/react-query";
import { Activity, AlertTriangle, Cpu, HardDrive, MemoryStick } from "lucide-react";

import { machineQuery } from "../api/client";
import type { MachineLevel, MachineView } from "../api/types";
import { Card } from "./ui/card";
import { cn } from "../lib/utils";

/** Tailwind tones per saturation level — the dashboard's ok/warn/urgent signal scale. */
export const LEVEL_TONE: Record<MachineLevel, { text: string; bar: string; badge: string }> = {
  ok: { text: "text-ok", bar: "bg-ok", badge: "border-ok/30 bg-ok-muted text-ok" },
  warn: { text: "text-warn", bar: "bg-warn", badge: "border-warn/30 bg-warn-muted text-warn" },
  critical: {
    text: "text-urgent",
    bar: "bg-urgent",
    badge: "border-urgent/30 bg-urgent-muted text-urgent",
  },
};

export const LEVEL_LABEL: Record<MachineLevel, string> = {
  ok: "healthy",
  warn: "warming",
  critical: "saturated",
};

/** Round MB to a compact GB string (`12.4 GB`). */
export function gb(mb: number): string {
  return `${(mb / 1024).toFixed(1)} GB`;
}

/** A labelled 0..1 gauge bar. `frac` null (unknown signal) renders a muted "—". `tone` colours the
 * fill once the value crosses into warn/critical; below that it stays neutral. */
export function Gauge({
  label,
  frac,
  detail,
  tone,
}: {
  label: React.ReactNode;
  frac: number | null;
  detail: string;
  tone: MachineLevel;
}) {
  const pct = frac == null ? null : Math.min(100, Math.max(0, frac * 100));
  return (
    <div className="space-y-1">
      <div className="flex items-baseline justify-between text-xs">
        <span className="text-muted-foreground">{label}</span>
        <span className="font-mono tabular-nums text-foreground">{detail}</span>
      </div>
      <div className="h-1.5 overflow-hidden rounded-full bg-muted">
        {pct != null && (
          <div
            className={cn("h-full rounded-full transition-all", LEVEL_TONE[tone].bar)}
            style={{ width: `${pct}%` }}
          />
        )}
      </div>
    </div>
  );
}

/** The gauge stripped to a bare 52×4px track — the topbar form, where a metric is one glance wide
 * and its label rides beside it rather than above. Same tone contract as `Gauge`, so a warming host
 * reads identically in the topbar and on the machine card. */
export function Meter({ frac, tone }: { frac: number | null; tone: MachineLevel }) {
  const pct = frac == null ? null : Math.min(100, Math.max(0, frac * 100));
  return (
    <span className="block h-1 w-[52px] overflow-hidden rounded-full bg-muted">
      {pct != null && (
        <span
          className={cn("block h-full rounded-full transition-all", LEVEL_TONE[tone].bar)}
          style={{ width: `${pct}%` }}
        />
      )}
    </span>
  );
}

/** The tone a single metric fraction earns on its own, so its gauge fill matches its severity even
 * when the overall card level is driven by a different signal. Thresholds mirror
 * `bn_core::machine` (load/core .7/1.0, RAM .85/.95, swap .10/.50). */
export function gaugeTone(frac: number | null, warn: number, crit: number): MachineLevel {
  if (frac == null) return "ok";
  if (frac >= crit) return "critical";
  if (frac >= warn) return "warn";
  return "ok";
}

/** Machine-health widget (#525): a compact, live, colour-coded card showing the host the fleet
 * runs on — CPU load, RAM, swap (the real OOM signal), and active agent count — with a glanceable
 * near-saturation warning so the human sees we're close *before* things melt. */
export function MachineHealth({ className }: { className?: string }) {
  const q = useQuery(machineQuery());
  const m = q.data as MachineView | undefined;

  if (q.isLoading || !m) {
    return (
      <Card className={cn("p-3", className)} aria-label="Machine health">
        <div className="text-xs text-muted-foreground">Loading machine health…</div>
      </Card>
    );
  }

  const level = m.saturation.level;
  const tone = LEVEL_TONE[level];
  const loadTone = gaugeTone(m.cpu.load_per_core, 0.7, 1.0);
  const ramTone = gaugeTone(m.ram.used_frac, 0.85, 0.95);
  const swapTone = gaugeTone(m.ram.swap_used_frac, 0.1, 0.5);
  const load1 = m.cpu.load1 == null ? "—" : m.cpu.load1.toFixed(2);

  return (
    <Card className={cn("space-y-3 p-3", className)} aria-label="Machine health">
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-1.5 text-sm font-semibold">
          <Activity className={cn("h-4 w-4", tone.text)} />
          Machine
        </div>
        <span
          className={cn(
            "inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-xs font-medium",
            tone.badge,
          )}
        >
          {level !== "ok" && <AlertTriangle className="h-3 w-3" />}
          {LEVEL_LABEL[level]}
        </span>
      </div>

      <div className="space-y-2.5">
        <Gauge
          label={
            <span className="inline-flex items-center gap-1">
              <Cpu className="h-3 w-3" /> CPU load
            </span>
          }
          frac={m.cpu.load_per_core}
          detail={`${load1} / ${m.cpu.cores} cores`}
          tone={loadTone}
        />
        <Gauge
          label={
            <span className="inline-flex items-center gap-1">
              <MemoryStick className="h-3 w-3" /> RAM
            </span>
          }
          frac={m.ram.used_frac}
          detail={`${gb(m.ram.used_mb)} / ${gb(m.ram.total_mb)}`}
          tone={ramTone}
        />
        <Gauge
          label={
            <span className="inline-flex items-center gap-1">
              <HardDrive className="h-3 w-3" /> Swap
            </span>
          }
          frac={m.ram.swap_used_frac}
          detail={
            m.ram.swap_total_mb === 0
              ? "none"
              : `${gb(m.ram.swap_used_mb)} / ${gb(m.ram.swap_total_mb)}`
          }
          tone={swapTone}
        />
      </div>

      <div className="flex items-center justify-between text-xs text-muted-foreground">
        <span>
          {m.agents.active} agent{m.agents.active === 1 ? "" : "s"} loading it
        </span>
      </div>

      {m.saturation.reasons.length > 0 && (
        <div className={cn("rounded-md border px-2 py-1.5 text-xs", tone.badge)}>
          {m.saturation.reasons.join(" · ")}
        </div>
      )}
    </Card>
  );
}
