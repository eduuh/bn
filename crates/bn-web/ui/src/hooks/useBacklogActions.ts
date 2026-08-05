// The backlog page's write layer: one react-query mutation per endpoint, each applying the design
// §4 optimistic rule — patch the `['orchestrate']` cache locally at once, then let the next SSE
// `orchestrate` frame (or the `onSettled` refetch) reconcile, rolling back on error. Every write
// reports success / failure through the toast `push`.

import { useRef } from "react";
import { useMutation, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { queryKeys } from "../api/client";
import type { OrchestrateView } from "../api/types";
import {
  applyQueuedOrder,
  optimisticItem,
  patchRun,
  recount,
} from "../lib/backlog";
import {
  apiDispatch,
  apiQueueAdd,
  apiQueueRemove,
  apiQueueReorder,
  apiScale,
  apiStart,
  apiStop,
} from "../api/writes";
import type { ToastKind } from "./useToasts";

type Push = (kind: ToastKind, text: string) => void;
interface Ctx {
  prev?: OrchestrateView;
  optimistic?: OrchestrateView;
}

/** Resolve the run an unscoped write targets: the explicit repo, else the sole live run's repo. */
function soleRepo(qc: QueryClient, explicit?: string): string | undefined {
  if (explicit) return explicit;
  const runs = qc.getQueryData<OrchestrateView>(queryKeys.orchestrate)?.runs ?? [];
  return runs.length === 1 ? runs[0].repo : undefined;
}

export interface BacklogActions {
  addToQueue: (v: { repo?: string; items: string[] }) => void;
  removeItem: (repo: string, id: string) => void;
  reorderQueue: (repo: string, ids: string[]) => void;
  setConcurrency: (repo: string, concurrency: number) => void;
  stopRun: (repo: string) => void;
  startRun: (v: { repo?: string; items: string[]; concurrency?: number }) => void;
  dispatchNow: (v: { repo: string; items: string[]; harness: string; kind: string }) => void;
  pending: Record<
    "add" | "remove" | "reorder" | "scale" | "stop" | "start" | "dispatch",
    boolean
  >;
}

export function useBacklogActions(push: Push): BacklogActions {
  const qc = useQueryClient();
  // Monotonic sequence for optimistic tmp-item ids so a same-second re-add can't collide.
  const seq = useRef(0);

  const fail = (e: unknown, fallback: string) => {
    push("error", e instanceof Error ? e.message : fallback);
  };

  /** Snapshot + patch `['orchestrate']` for an optimistic mutation; returns the rollback context. */
  const beginPatch = async (patch: (v: OrchestrateView) => OrchestrateView): Promise<Ctx> => {
    await qc.cancelQueries({ queryKey: queryKeys.orchestrate });
    const prev = qc.getQueryData<OrchestrateView>(queryKeys.orchestrate);
    let optimistic: OrchestrateView | undefined;
    if (prev) {
      optimistic = patch(prev);
      qc.setQueryData<OrchestrateView>(queryKeys.orchestrate, optimistic);
    }
    return { prev, optimistic };
  };
  const rollback = (ctx?: Ctx) => {
    if (!ctx?.prev) return;
    // Only revert if our optimistic value is still the one in the cache; a newer SSE frame or a
    // later mutation is fresher truth and must not be clobbered back to the pre-mutation snapshot.
    if (qc.getQueryData<OrchestrateView>(queryKeys.orchestrate) === ctx.optimistic) {
      qc.setQueryData(queryKeys.orchestrate, ctx.prev);
    }
  };
  const reconcile = () => {
    void qc.invalidateQueries({ queryKey: queryKeys.orchestrate });
  };

  const add = useMutation<unknown, unknown, { repo?: string; items: string[] }, Ctx>({
    mutationFn: (v) => apiQueueAdd(v),
    onMutate: (v) => {
      const repo = soleRepo(qc, v.repo);
      if (!repo) return {};
      const now = Math.floor(Date.now() / 1000);
      return beginPatch((view) =>
        patchRun(view, repo, (run) =>
          recount({
            ...run,
            queue: [...run.queue, ...v.items.map((it) => optimisticItem(it, seq.current++, now))],
          }),
        ),
      );
    },
    onError: (e, _v, ctx) => {
      rollback(ctx);
      fail(e, "could not queue items");
    },
    onSuccess: (_d, v) => push("ok", `Queued ${v.items.length} item${v.items.length === 1 ? "" : "s"}`),
    onSettled: reconcile,
  });

  const remove = useMutation<unknown, unknown, { repo: string; id: string }, Ctx>({
    mutationFn: (v) => apiQueueRemove(v.id, v.repo),
    onMutate: (v) =>
      beginPatch((view) =>
        patchRun(view, v.repo, (run) =>
          recount({ ...run, queue: run.queue.filter((i) => i.id !== v.id) }),
        ),
      ),
    onError: (e, _v, ctx) => {
      rollback(ctx);
      fail(e, "could not remove item");
    },
    onSuccess: () => push("ok", "Removed queued item"),
    onSettled: reconcile,
  });

  const reorder = useMutation<unknown, unknown, { repo: string; ids: string[] }, Ctx>({
    mutationFn: (v) => apiQueueReorder(v),
    onMutate: (v) =>
      beginPatch((view) =>
        patchRun(view, v.repo, (run) => ({ ...run, queue: applyQueuedOrder(run.queue, v.ids) })),
      ),
    onError: (e, _v, ctx) => {
      rollback(ctx);
      fail(e, "could not reorder queue");
    },
    onSettled: reconcile,
  });

  const scale = useMutation<unknown, unknown, { repo: string; concurrency: number }, Ctx>({
    mutationFn: (v) => apiScale(v),
    onMutate: (v) =>
      beginPatch((view) =>
        patchRun(view, v.repo, (run) => ({ ...run, concurrency: v.concurrency })),
      ),
    onError: (e, _v, ctx) => {
      rollback(ctx);
      fail(e, "could not scale");
    },
    onSuccess: (_d, v) => push("ok", `Concurrency → ${v.concurrency}`),
    onSettled: reconcile,
  });

  const stop = useMutation<unknown, unknown, { repo: string }, Ctx>({
    mutationFn: (v) => apiStop(v),
    onMutate: (v) =>
      beginPatch((view) => patchRun(view, v.repo, (run) => ({ ...run, stopped: true }))),
    onError: (e, _v, ctx) => {
      rollback(ctx);
      fail(e, "could not stop the daemon");
    },
    onSuccess: () => push("ok", "Stopping the daemon"),
    onSettled: reconcile,
  });

  const start = useMutation<unknown, unknown, { repo?: string; items: string[]; concurrency?: number }>({
    mutationFn: (v) => apiStart(v),
    onError: (e) => fail(e, "could not start the daemon"),
    onSuccess: (_d, v) =>
      push("ok", `Started a run with ${v.items.length} item${v.items.length === 1 ? "" : "s"}`),
    onSettled: reconcile,
  });

  const dispatch = useMutation<
    number,
    unknown,
    { repo: string; items: string[]; harness: string; kind: string }
  >({
    mutationFn: async (v) => {
      const results = await Promise.allSettled(
        v.items.map((item) =>
          apiDispatch({ repo: v.repo, item, harness: v.harness, kind: v.kind }),
        ),
      );
      const rejected = results.find((r) => r.status === "rejected") as
        | PromiseRejectedResult
        | undefined;
      if (rejected) {
        throw rejected.reason instanceof Error ? rejected.reason : new Error("dispatch failed");
      }
      return v.items.length;
    },
    onError: (e) => fail(e, "could not dispatch"),
    onSuccess: (n) => push("ok", `Dispatched ${n} agent${n === 1 ? "" : "s"}`),
  });

  return {
    addToQueue: (v) => add.mutate(v),
    removeItem: (repo, id) => remove.mutate({ repo, id }),
    reorderQueue: (repo, ids) => reorder.mutate({ repo, ids }),
    setConcurrency: (repo, concurrency) => scale.mutate({ repo, concurrency }),
    stopRun: (repo) => stop.mutate({ repo }),
    startRun: (v) => start.mutate(v),
    dispatchNow: (v) => dispatch.mutate(v),
    pending: {
      add: add.isPending,
      remove: remove.isPending,
      reorder: reorder.isPending,
      scale: scale.isPending,
      stop: stop.isPending,
      start: start.isPending,
      dispatch: dispatch.isPending,
    },
  };
}
