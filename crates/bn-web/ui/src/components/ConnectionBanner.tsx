import type { StreamStatus } from "../hooks/useBnStream";
import { cn } from "../lib/utils";

/** The top connection banner (#499). Hidden while live (`open`/`connecting`). A transient blip
 * (`lost`) shows a soft "reconnecting…"; a real outage (`down`) shows an honest "server is down"
 * with the auto-retry made explicit — no infinite silent spinner. Both clear themselves the moment
 * the stream reopens (the hook flips back to `open` and invalidates the stale data). */
export function ConnectionBanner({ status }: { status: StreamStatus }) {
  if (status !== "lost" && status !== "down") return null;
  const down = status === "down";
  return (
    <div
      className={cn(
        "border-b px-4 py-2 text-center text-sm",
        down
          ? "border-urgent bg-urgent-muted text-[#ffb4ab]"
          : "border-warn bg-warn-muted text-warn",
      )}
      role="status"
      aria-live="polite"
    >
      {down
        ? "⚠ Server is down — retrying automatically. Check the bn-web service (bn serve status)."
        : "Live connection lost — reconnecting…"}
    </div>
  );
}
