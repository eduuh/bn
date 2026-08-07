// The backlog page's write layer. Each endpoint is one `useAppMutation`, so these writes now get
// the same lifecycle as every other button in the dashboard: patch `['orchestrate']` locally at
// once, let the next SSE `orchestrate` frame (or the reconcile on settle) confirm it, roll back on
// error, and report through the shared toast store.
//
// This file used to hand-roll that lifecycle, and its rollback silently never fired: it decided
// "is this still my write?" by comparing object identity, but `setQueryData` runs values through
// react-query's structural sharing and stores a rebuilt object, so the comparison was always false.
// `useAppMutation` claims its writes by timestamp instead — see the note in that file.

import { useRef } from "react";
import { useQueryClient, type QueryClient } from "@tanstack/react-query";
import { queryKeys } from "../api/client";
import type { OrchestrateView } from "../api/types";
import { applyQueuedOrder, optimisticItem, patchRun, recount } from "../lib/backlog";
import { useAppMutation, optimistic } from "./useAppMutation";
import { affects } from "../lib/invalidation";
import {
  apiDispatch,
  apiQueueAdd,
  apiQueueRemove,
  apiQueueReorder,
  apiScale,
  apiStart,
  apiStop,
} from "../api/writes";

/** Resolve the run an unscoped write targets: the explicit repo, else the sole live run's repo. */
function soleRepo(qc: QueryClient, explicit?: string): string | undefined {
  if (explicit) return explicit;
  const runs = qc.getQueryData<OrchestrateView>(queryKeys.orchestrate)?.runs ?? [];
  return runs.length === 1 ? runs[0].repo : undefined;
}

/** Patch one run inside `['orchestrate']`, resolving the target repo from the write's vars. */
function patchRunOf<TVars>(
  repoOf: (vars: TVars) => string | undefined,
  edit: (run: OrchestrateView["runs"][number], vars: TVars) => OrchestrateView["runs"][number],
) {
  return optimistic<OrchestrateView, TVars>(queryKeys.orchestrate, (view, vars) => {
    const repo = repoOf(vars);
    return repo ? patchRun(view, repo, (run) => edit(run, vars)) : view;
  });
}

export interface BacklogActions {
  addToQueue: (v: { repo?: string; items: string[] }) => void;
  removeItem: (repo: string, id: string) => void;
  reorderQueue: (repo: string, ids: string[]) => void;
  setConcurrency: (repo: string, concurrency: number) => void;
  stopRun: (repo: string) => void;
  startRun: (v: { repo?: string; items: string[]; concurrency?: number }) => void;
  dispatchNow: (v: { repo: string; items: string[]; harness: string; kind: string }) => void;
  pending: Record<"add" | "remove" | "reorder" | "scale" | "stop" | "start" | "dispatch", boolean>;
}

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

export function useBacklogActions(): BacklogActions {
  const qc = useQueryClient();
  // Monotonic sequence for optimistic tmp-item ids so a same-second re-add can't collide.
  const seq = useRef(0);

  const add = useAppMutation<unknown, { repo?: string; items: string[] }>({
    mutationFn: (v) => apiQueueAdd(v),
    optimistic: patchRunOf(
      (v) => soleRepo(qc, v.repo),
      (run, v) => {
        const now = Math.floor(Date.now() / 1000);
        return recount({
          ...run,
          queue: [...run.queue, ...v.items.map((it) => optimisticItem(it, seq.current++, now))],
        });
      },
    ),
    invalidates: affects.backlog,
    toast: {
      success: (_d, v) => `Queued ${plural(v.items.length, "item")}`,
      error: "Could not queue items",
    },
  });

  const remove = useAppMutation<unknown, { repo: string; id: string }>({
    mutationFn: (v) => apiQueueRemove(v.id, v.repo),
    optimistic: patchRunOf(
      (v) => v.repo,
      (run, v) => recount({ ...run, queue: run.queue.filter((i) => i.id !== v.id) }),
    ),
    invalidates: affects.backlog,
    toast: { success: "Removed queued item", error: "Could not remove item" },
  });

  const reorder = useAppMutation<unknown, { repo: string; ids: string[] }>({
    mutationFn: (v) => apiQueueReorder(v),
    optimistic: patchRunOf(
      (v) => v.repo,
      (run, v) => ({ ...run, queue: applyQueuedOrder(run.queue, v.ids) }),
    ),
    invalidates: affects.backlog,
    toast: { error: "Could not reorder queue" },
  });

  const scale = useAppMutation<unknown, { repo: string; concurrency: number }>({
    mutationFn: (v) => apiScale(v),
    optimistic: patchRunOf(
      (v) => v.repo,
      (run, v) => ({ ...run, concurrency: v.concurrency }),
    ),
    invalidates: affects.backlog,
    toast: {
      success: (_d, v) => `Concurrency → ${v.concurrency}`,
      error: "Could not scale",
    },
  });

  const stop = useAppMutation<unknown, { repo: string }>({
    mutationFn: (v) => apiStop(v),
    optimistic: patchRunOf(
      (v) => v.repo,
      (run) => ({ ...run, stopped: true }),
    ),
    invalidates: affects.backlog,
    toast: { success: "Stopping the daemon", error: "Could not stop the daemon" },
  });

  // Starting a run mints the run itself, so there is nothing in the cache to patch yet.
  const start = useAppMutation<unknown, { repo?: string; items: string[]; concurrency?: number }>({
    mutationFn: (v) => apiStart(v),
    invalidates: affects.backlog,
    toast: {
      success: (_d, v) => `Started a run with ${plural(v.items.length, "item")}`,
      error: "Could not start the daemon",
    },
  });

  const dispatch = useAppMutation<
    number,
    { repo: string; items: string[]; harness: string; kind: string }
  >({
    mutationFn: async (v) => {
      const results = await Promise.allSettled(
        v.items.map((item) =>
          apiDispatch({ repo: v.repo, item, harness: v.harness, kind: v.kind }),
        ),
      );
      const rejected = results.find((r) => r.status === "rejected");
      if (rejected) {
        throw rejected.reason instanceof Error ? rejected.reason : new Error("dispatch failed");
      }
      return v.items.length;
    },
    invalidates: affects.agentLaunch,
    toast: { success: (n) => `Dispatched ${plural(n, "agent")}`, error: "Could not dispatch" },
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
