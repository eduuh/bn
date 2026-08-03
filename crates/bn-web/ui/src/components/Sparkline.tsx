import { useEffect, useRef } from "react";
import uPlot from "uplot";
import "uplot/dist/uPlot.min.css";
import { mergeSparkline } from "../lib/board";
import type { RunView } from "../api/types";

/** A tiny canvas sparkline of merges-per-hour over the last 12 hours (uPlot: fast, no axes/legend).
 * Created once, then fed new data on change so the plot instance is reused rather than rebuilt. */
export function Sparkline({ run, now }: { run: RunView; now: number }) {
  const elRef = useRef<HTMLDivElement>(null);
  const plotRef = useRef<uPlot | null>(null);
  const { xs, ys } = mergeSparkline(run, now);

  useEffect(() => {
    if (!elRef.current) return;
    const opts: uPlot.Options = {
      width: 240,
      height: 40,
      cursor: { show: false },
      legend: { show: false },
      scales: { x: { time: false } },
      axes: [{ show: false }, { show: false }],
      series: [
        {},
        {
          stroke: "#3fb950",
          fill: "rgba(63,185,80,0.16)",
          width: 1.5,
          points: { show: false },
        },
      ],
    };
    const u = new uPlot(opts, [xs.slice(), ys.slice()], elRef.current);
    plotRef.current = u;
    return () => {
      u.destroy();
      plotRef.current = null;
    };
    // Build once; data updates flow through the second effect.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    plotRef.current?.setData([xs, ys]);
  }, [xs, ys]);

  return <div className="h-10 w-full" ref={elRef} aria-label="throughput sparkline" />;
}
