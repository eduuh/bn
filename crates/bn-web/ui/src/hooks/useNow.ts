import { useEffect, useState } from "react";

/** A `now` (epoch seconds) that re-ticks on an interval, so relative "3m ago" labels stay fresh
 * without every card owning its own timer. Defaults to a 30s cadence. */
export function useNow(intervalMs = 30_000): number {
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000));
  useEffect(() => {
    const id = window.setInterval(() => setNow(Math.floor(Date.now() / 1000)), intervalMs);
    return () => window.clearInterval(id);
  }, [intervalMs]);
  return now;
}
