import type { RunView } from "../../api/types";
import { Badge } from "../ui/badge";
import { Button } from "../ui/button";
import { Stepper } from "./controls";

/**
 * The lanes panel (design §4(d)): the live concurrency stepper (`POST scale`, honored on the next
 * tick — the #385 adjust-mid-run), a solo indicator, the stopped/drained state from the live
 * status frame, and the per-run Stop control. A single cap today; typed coding/research steppers
 * slot in here when #385's lanes land in `OrchestrateState`.
 */
export function LanesPanel({
  run,
  busy,
  onScale,
  onStop,
}: {
  run: RunView;
  busy: boolean;
  onScale: (concurrency: number) => void;
  onStop: () => void;
}) {
  const effective = run.solo ? 1 : run.concurrency;
  return (
    <div className="mb-3 rounded-md border border-border bg-card p-4">
      <div className="flex flex-wrap items-center gap-4">
        <div className="text-sm">
          <span className="block font-semibold">Concurrency</span>
        </div>
        <Stepper
          value={run.concurrency}
          disabled={busy || run.stopped}
          onChange={onScale}
          label="concurrency"
        />
        <span className="text-xs text-muted-foreground">lanes the daemon runs in parallel</span>
      </div>

      <div className="mt-3 flex flex-wrap items-center gap-2">
        {run.solo && (
          <Badge variant="warn" title="solo → one lane regardless of cap">
            solo · {effective}
          </Badge>
        )}
        {run.stopped && <Badge variant="urgent">stopped</Badge>}
        {run.drained && <Badge variant="muted">drained</Badge>}
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="border-urgent/40 text-urgent hover:bg-urgent-muted hover:text-urgent"
          onClick={onStop}
          disabled={busy || run.stopped}
          title="stop the daemon (next tick exits + launchd unload)"
        >
          Stop
        </Button>
      </div>
    </div>
  );
}
