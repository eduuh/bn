import type { MachineLevel } from "../api/types";

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

/** The tone a single metric fraction earns on its own, so its gauge fill matches its severity even
 * when the overall card level is driven by a different signal. Thresholds mirror
 * `bn_core::machine` (load/core .7/1.0, RAM .85/.95, swap .10/.50). */
export function gaugeTone(frac: number | null, warn: number, crit: number): MachineLevel {
  if (frac == null) return "ok";
  if (frac >= crit) return "critical";
  if (frac >= warn) return "warn";
  return "ok";
}
