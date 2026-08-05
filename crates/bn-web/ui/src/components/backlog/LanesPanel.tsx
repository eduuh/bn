import type { RunView } from "../../api/types";
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
    <div className="lanes-panel">
      <div className="lanes-main">
        <div className="lanes-label">
          <span className="lanes-title">Concurrency</span>
          <span className="sub">lanes the daemon runs in parallel</span>
        </div>
        <Stepper
          value={run.concurrency}
          disabled={busy || run.stopped}
          onChange={onScale}
          label="concurrency"
        />
      </div>

      <div className="lanes-state">
        {run.solo && (
          <span className="chip chip-amber" title="solo → one lane regardless of cap">
            solo · {effective}
          </span>
        )}
        {run.stopped && <span className="chip chip-red">stopped</span>}
        {run.drained && <span className="chip chip-grey">drained</span>}
        <button
          type="button"
          className="btn btn-danger"
          onClick={onStop}
          disabled={busy || run.stopped}
          title="stop the daemon (next tick exits + launchd unload)"
        >
          Stop
        </button>
      </div>
    </div>
  );
}
