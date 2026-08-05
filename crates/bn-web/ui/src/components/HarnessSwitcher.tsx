import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Ban } from "lucide-react";
import { queryKeys } from "../api/client";
import type { HarnessOption } from "../api/types";
import { switchHarness } from "../api/writes";
import { pushToast } from "../lib/toast";
import { cn } from "../lib/utils";

/** Fallback options when the fleet read hasn't carried availability yet — copilot always usable,
 * claude assumed usable (the server re-checks and rejects a disabled one anyway). */
const DEFAULT_HARNESSES: HarnessOption[] = [
  { name: "copilot", available: true },
  { name: "claude", available: true },
];

/**
 * Per-agent harness switcher (#501): a segmented control of the harnesses this agent can run
 * under, the current one highlighted. Picking a different *available* harness confirms, then
 * `POST /relaunch {harness}` — bn stops the agent and relaunches it in place (same worktree/branch,
 * note re-seeded) under the new harness. An unavailable harness (e.g. an org-disabled claude)
 * renders disabled with its reason as a tooltip, never a silent drop. Optimistic toast; the
 * fleet/pane invalidation converges it on the next tick. Buttons stop propagation so the switcher
 * works inside the card's navigation link. Mobile-first: ≥44px touch targets.
 */
export function HarnessSwitcher({
  target,
  current,
  harnesses,
  label = "Harness",
}: {
  target: string;
  current?: string;
  harnesses?: HarnessOption[];
  label?: string;
}) {
  const qc = useQueryClient();
  const options = harnesses && harnesses.length > 0 ? harnesses : DEFAULT_HARNESSES;

  const mut = useMutation({
    mutationFn: (harness: string) => switchHarness(target, harness),
    onSuccess: (_data, harness) => {
      pushToast(`restarting ${target} under ${harness}…`, "success");
      void qc.invalidateQueries({ queryKey: queryKeys.fleet });
      void qc.invalidateQueries({ queryKey: queryKeys.pane(target) });
    },
    onError: (e) => {
      pushToast(`switch failed: ${(e as Error).message}`, "error");
    },
  });

  const pick = (opt: HarnessOption) => {
    if (mut.isPending || opt.name === current) return;
    if (!opt.available) {
      pushToast(opt.reason ?? `${opt.name} is unavailable`, "error");
      return;
    }
    if (
      window.confirm(
        `Relaunch ${target} under ${opt.name}? Same worktree/branch — the note is preserved.`,
      )
    ) {
      mut.mutate(opt.name);
    }
  };

  return (
    <div role="group" aria-label={`${label}: switch harness`} className="space-y-1.5">
      <span className="text-xs font-medium text-muted-foreground">{label}</span>
      <div className="grid grid-flow-col auto-cols-fr gap-1 rounded-lg bg-muted p-1">
        {options.map((opt) => {
          const active = opt.name === current;
          const title = !opt.available
            ? opt.reason ?? `${opt.name} is unavailable`
            : active
              ? `Running under ${opt.name}`
              : `Relaunch under ${opt.name}`;
          return (
            <button
              key={opt.name}
              type="button"
              data-active={active ? "true" : undefined}
              aria-pressed={active}
              disabled={mut.isPending || (!opt.available && !active)}
              title={title}
              className={cn(
                "inline-flex min-h-[2.75rem] items-center justify-center gap-1.5 rounded-md px-3 py-2 text-sm font-medium transition-colors",
                active
                  ? "bg-card text-foreground shadow-card"
                  : "text-muted-foreground hover:text-foreground",
                !opt.available && "opacity-60",
              )}
              onClick={(e) => {
                e.preventDefault();
                e.stopPropagation();
                pick(opt);
              }}
            >
              {opt.name}
              {!opt.available && <Ban className="h-3.5 w-3.5" aria-hidden="true" />}
            </button>
          );
        })}
      </div>
    </div>
  );
}
