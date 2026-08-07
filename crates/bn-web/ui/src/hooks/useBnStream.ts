import { useEffect, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { queryKeys } from "../api/client";
import type { FleetView, OrchestrateView } from "../api/types";
import { classifyConn, HEALTH_PROBE_MS, type ConnStatus } from "../lib/conn";

/** The dashboard connection state (see `../lib/conn`): `connecting` before the first open, `open`
 * while live, `lost` for a transient blip (stream dropped, server still answering), `down` once
 * health probes fail past the grace — an honest outage, not an eternal silent "reconnecting". */
export type StreamStatus = ConnStatus;

/** Probe `GET /api/health`; a 2xx means the server is up (the SSE gap is transient). Any non-2xx or
 * transport error counts as a failure toward the outage escalation in `../lib/conn`. */
async function healthOk(): Promise<boolean> {
  try {
    const res = await fetch("/api/health", { cache: "no-store" });
    return res.ok;
  } catch {
    return false;
  }
}

/**
 * Owns the single `EventSource` on `/api/stream` for the whole app. Each named frame is a raw
 * viewmodel (design §3) — the exact `.data` shape the REST endpoints return — so writing it
 * straight into the react-query cache keeps `fleet`/`orchestrate` live with no per-component
 * fetching. The `orchestrate` frame also invalidates the recovery feed (a run file only rewrites
 * when its queue/recovery actually changed). Components just `useQuery` those keys and re-render.
 *
 * Honest connection state (#499): the browser's `EventSource` reconnects on its own, but a dead
 * server would leave it spinning forever. So on a drop we poll `/api/health` and escalate `lost` →
 * `down` once probes keep failing (see `../lib/conn`), and on recovery we invalidate every query so
 * the dashboard catches up on whatever changed during the gap.
 */
export function useBnStream(): { status: StreamStatus } {
  const qc = useQueryClient();
  const [status, setStatus] = useState<StreamStatus>("connecting");

  useEffect(() => {
    let streamOpen = false;
    let hadDrop = false;
    let failures = 0;
    let probeTimer: ReturnType<typeof setInterval> | null = null;
    let probing = false;

    const stopProbe = () => {
      if (probeTimer !== null) {
        clearInterval(probeTimer);
        probeTimer = null;
      }
    };

    const probeHealth = async () => {
      if (probing) return; // don't stack probes on a slow/dead server
      probing = true;
      const ok = await healthOk();
      probing = false;
      if (streamOpen) {
        // The stream came back mid-probe; `onopen` owns the state now.
        stopProbe();
        return;
      }
      failures = ok ? 0 : failures + 1;
      setStatus(classifyConn(false, failures));
    };

    const startProbe = () => {
      // `void` rather than passing the async fn straight to setInterval: a rejected probe would
      // otherwise become an unhandled rejection, and setInterval discards the promise anyway.
      if (probeTimer === null) probeTimer = setInterval(() => void probeHealth(), HEALTH_PROBE_MS);
    };

    const es = new EventSource("/api/stream");

    es.onopen = () => {
      streamOpen = true;
      failures = 0;
      stopProbe();
      setStatus("open");
      if (hadDrop) {
        // Recovered after a gap: pull fresh data for whatever changed while we were disconnected.
        void qc.invalidateQueries();
        hadDrop = false;
      }
    };
    es.onerror = () => {
      streamOpen = false;
      hadDrop = true;
      // Immediately a transient blip; the health probe escalates to `down` past the grace.
      setStatus((s) => (s === "down" ? "down" : "lost"));
      startProbe();
    };

    // `MessageEvent<string>`, not bare `MessageEvent`: `data` is `any` on the unparameterised
    // form, which silently makes every JSON.parse below untyped.
    const onFleet = (e: MessageEvent<string>) => {
      try {
        qc.setQueryData<FleetView>(queryKeys.fleet, JSON.parse(e.data) as FleetView);
      } catch {
        /* a torn frame is ignored; the next frame or the fallback refetch reconciles */
      }
    };
    const onOrchestrate = (e: MessageEvent<string>) => {
      try {
        qc.setQueryData<OrchestrateView>(
          queryKeys.orchestrate,
          JSON.parse(e.data) as OrchestrateView,
        );
        // The run file changed → its recovery log may have grown; pull the deeper feed.
        void qc.invalidateQueries({ queryKey: queryKeys.recovery });
      } catch {
        /* ignore a torn frame */
      }
    };

    const onQueue = (e: MessageEvent<string>) => {
      try {
        qc.setQueryData(queryKeys.queue, JSON.parse(e.data));
        // The queue moved, so the day's projection did too — but the timeline is a 24-hour fold
        // that changes meaningfully once a minute, so invalidate rather than pushing a frame for it.
        void qc.invalidateQueries({ queryKey: ["queue-timeline"] });
        // The entry list is a different query (it carries every state, not just pending), so the
        // frame that updates the live view has to invalidate it too or history goes stale.
        void qc.invalidateQueries({ queryKey: ["queue", "entries"] });
      } catch {
        /* ignore a torn frame */
      }
    };

    /**
     * The event log, already streamed per line as a `log` frame — the events page was polling on a
     * timer while the thing it renders was arriving live on a channel it did not listen to.
     *
     * Invalidate rather than append: the page folds `dispatch` + `finish` into runs and applies
     * server-side filters, so a client-side push would have to reimplement both to stay consistent
     * with what a refetch returns. A refetch of a few hundred rows is cheap; two implementations of
     * the same fold that can disagree is not.
     *
     * Only the queue's own kinds — every note command writes a metric, and waking the page for
     * those would make it noisier than the timer it replaces.
     */
    const onLog = (e: MessageEvent<string>) => {
      try {
        const kind = (JSON.parse(e.data) as { kind?: string }).kind ?? "";
        if (kind.startsWith("queue_")) {
          void qc.invalidateQueries({ queryKey: ["queue", "events"] });
        }
      } catch {
        /* ignore a torn frame */
      }
    };

    es.addEventListener("fleet", onFleet);
    es.addEventListener("log", onLog);
    es.addEventListener("orchestrate", onOrchestrate);
    es.addEventListener("queue", onQueue);

    return () => {
      stopProbe();
      es.removeEventListener("fleet", onFleet);
      es.removeEventListener("log", onLog);
      es.removeEventListener("orchestrate", onOrchestrate);
      es.removeEventListener("queue", onQueue);
      es.close();
    };
  }, [qc]);

  return { status };
}
