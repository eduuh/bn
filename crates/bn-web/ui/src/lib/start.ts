// Pure Start-view logic (#531): fold the live runs + fleet into the intake form's defaults —
// which repo to prefill, which harness is already in flight, and per-harness availability — so the
// Start screen opens ready even when no daemon run is up. Side-effect-free + unit-tested
// (`start.test.ts`).

import type { FleetView, RunView } from "../api/types";

/** The two dispatchable harnesses the Start form offers. */
export type Harness = "claude" | "copilot";

/** Repos seen in the live runs / fleet — the repo combobox options (#343). */
export function knownRepos(runs: RunView[], fleet?: FleetView): string[] {
  const s = new Set<string>();
  for (const r of runs) if (r.repo) s.add(r.repo);
  for (const a of fleet?.agents ?? []) if (a.repo) s.add(a.repo);
  return [...s].sort();
}

/** The repo to prefill: the sole/first run's repo, else the first live-fleet repo (so Start opens
 * ready even when no daemon run is up), else empty. */
export function defaultRepo(runs: RunView[], fleet?: FleetView): string {
  return runs[0]?.repo ?? knownRepos(runs, fleet)[0] ?? "";
}

/** The harness a live agent is currently running under (the first claude/copilot agent), so Start
 * can default its selector to what's already in flight instead of a fixed guess. */
export function runningHarness(fleet?: FleetView): Harness | undefined {
  for (const a of fleet?.agents ?? []) {
    if (a.harness === "claude" || a.harness === "copilot") return a.harness;
  }
  return undefined;
}

/** Whether a harness is usable on this machine per the fleet read's availability list; unknown
 * (the read hasn't carried it yet) is treated as available — the server re-checks on dispatch. */
export function harnessAvailable(name: Harness, fleet?: FleetView): boolean {
  const h = fleet?.harnesses?.find((o) => o.name === name);
  return h ? h.available : true;
}

/** The reason a harness is unavailable, when the fleet read carries one (a tooltip). */
export function harnessReason(name: Harness, fleet?: FleetView): string | undefined {
  return fleet?.harnesses?.find((o) => o.name === name)?.reason ?? undefined;
}
