// Honest dashboard connection state (#499). Mirrors the pure policy in
// `crates/bn-core/src/serve.rs` (`classify_conn` + `DOWN_AFTER_FAILURES`); keep the grace in sync.
//
// While the SSE stream is open we're `open`. When it drops, the browser's EventSource silently
// auto-reconnects — but a dead server would leave it spinning "reconnecting…" forever, so we probe
// `GET /api/health` and escalate: still answering (within grace) → `lost` (a transient blip);
// failing past the grace → `down` (surface it honestly). `connecting` is the pre-first-open state.

/** The dashboard's connection state, driving the header dot and the top banner. */
export type ConnStatus = "connecting" | "open" | "lost" | "down";

/** How often (ms) to poll `/api/health` while the stream is down, to tell a blip from an outage.
 * Mirrors `bn_core::serve::HEALTH_PROBE_SECS`. */
export const HEALTH_PROBE_MS = 3000;

/** Consecutive failed health probes before an ongoing reconnect is called an honest outage.
 * Mirrors `bn_core::serve::DOWN_AFTER_FAILURES`. */
export const DOWN_AFTER_FAILURES = 3;

/**
 * Classify the connection from the live-stream state and the health-probe failure run. An open
 * stream is always `open`; otherwise the consecutive failed-probe count decides blip (`lost`) vs
 * outage (`down`) at the {@link DOWN_AFTER_FAILURES} threshold. Pure — the exact rule
 * `bn_core::serve::classify_conn` encodes on the Rust side.
 */
export function classifyConn(streamOpen: boolean, consecutiveFailures: number): ConnStatus {
  if (streamOpen) return "open";
  return consecutiveFailures >= DOWN_AFTER_FAILURES ? "down" : "lost";
}
