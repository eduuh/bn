// The one write primitive. Every mutation in the dashboard goes through this hook so that a click
// behaves the same way everywhere, whichever screen it happens on.
//
// Before this existed, each of the ~60 `useMutation` call sites hand-rolled its own lifecycle, and
// they disagreed in three ways that the user actually feels:
//
//   1. Almost none patched the cache, so a click sat visibly dead until the POST returned *and* the
//      invalidated read refetched — two round-trips of nothing happening.
//   2. Most invalidated in `onSuccess`, so a *failed* write never reconciled: the screen kept
//      showing pre-mutation state that the server had possibly already moved past.
//   3. Error toasts were spelled a different way at every site.
//
// So the rule this hook enforces, from `useBacklogActions`' design §4 pattern:
//
//   cancel in-flight reads → snapshot → patch → (rollback iff still ours) → always reconcile
//
// The "iff still ours" guard matters because `useBnStream` writes the same cache keys from SSE
// frames. If a frame landed between our patch and our failure, that frame is fresher truth than our
// pre-mutation snapshot, so rolling back would drag the UI *backwards*. We only revert when the
// value in the cache is still the exact object we put there.

import { useMutation, useQueryClient, type QueryKey } from "@tanstack/react-query";
import { pushToast } from "../lib/toast";

/**
 * A declared optimistic cache patch. Build one with {@link optimistic} rather than by hand — the
 * factory is what ties the cache type to the patch function; this interface deliberately erases it
 * so a single mutation can carry patches for several differently-typed caches.
 */
export interface OptimisticPatch<TVars> {
  /** The key to patch, or `undefined` to skip (the target isn't knowable from these vars). */
  readonly key: (vars: TVars) => QueryKey | undefined;
  readonly patch: (prev: unknown, vars: TVars) => unknown;
}

/**
 * Declare an optimistic patch for one cache key.
 *
 * `TCache` is the viewmodel stored under the key; the patch is pure and must return a *new* object
 * (identity is how rollback tells our write apart from a later SSE frame).
 *
 * ```ts
 * optimistic<AttnView, { id: string }>(queryKeys.attn, (view, v) => ({
 *   ...view,
 *   items: view.items.filter((i) => i.id !== v.id),
 * }))
 * ```
 */
export function optimistic<TCache, TVars = void>(
  key: QueryKey | ((vars: TVars) => QueryKey | undefined),
  patch: (prev: TCache, vars: TVars) => TCache,
): OptimisticPatch<TVars> {
  // A QueryKey is always an array, so `typeof === "function"` cleanly separates the two forms.
  const resolve = typeof key === "function" ? key : () => key;
  return { key: resolve, patch: (prev, vars) => patch(prev as TCache, vars) };
}

/** What a toast says. A plain string for the error case is treated as a prefix on the error's own
 * message — `"Could not dismiss"` renders as `Could not dismiss: <reason>` — because that is what
 * nearly every hand-written call site was already doing. Return `undefined` from the function form
 * to stay silent for that outcome. */
export interface ToastSpec<TData, TVars> {
  success?: string | ((data: TData, vars: TVars) => string | undefined);
  error?: string | ((err: Error, vars: TVars) => string | undefined);
}

export interface AppMutationOptions<TData, TVars> {
  mutationFn: (vars: TVars) => Promise<TData>;
  /** Cache patches applied before the request goes out, rolled back if it fails. */
  optimistic?: OptimisticPatch<TVars> | OptimisticPatch<TVars>[];
  /**
   * Keys to refetch once the write settles — on failure as well as success, so a rejected write
   * always leaves the screen showing server truth rather than a stale guess. Matching is by prefix,
   * so `["tasks"]` reconciles every task scope. Any key named in `optimistic` is reconciled
   * automatically and does not need repeating here.
   */
  invalidates?: QueryKey[] | ((vars: TVars) => QueryKey[]);
  /** Keep declared invalidations success-only when a failed refetch would overwrite unsaved local
   * input. Optimistically patched keys still reconcile on failure. Defaults to `true`. */
  invalidateOnError?: boolean;
  toast?: ToastSpec<TData, TVars>;
  /** Escape hatches for side effects a call site still needs — navigation, closing a dialog,
   * clearing a composer. Run after this hook's own toast/rollback handling. */
  onSuccess?: (data: TData, vars: TVars) => void;
  onError?: (err: Error, vars: TVars) => void;
}

interface Snapshot {
  key: QueryKey;
  prev: unknown;
  /** The `dataUpdatedAt` the cache carried immediately after our patch — our claim ticket. */
  stamp: number;
}
interface Ctx {
  snapshots: Snapshot[];
}

// Identity is not usable for "is this still my write?". `setQueryData` runs the new value through
// react-query's structural sharing (`replaceEqualDeep`), which returns a freshly constructed object
// whenever anything differs — so the object you hand in is generally *not* the object the cache
// ends up holding, and `getQueryData(key) === myValue` is false even when nothing else has written.
// (The hand-rolled guard in the old backlog actions had exactly this bug: its rollback never fired.)
//
// So we claim our write by timestamp instead. Each optimistic patch is stored with an explicit,
// strictly increasing `updatedAt`; if the cache still reports that exact value when the request
// fails, nobody has written since and the rollback is safe.
let clock = 0;
function stamp(): number {
  clock = Math.max(clock + 1, Date.now());
  return clock;
}

function asArray<T>(v: T | T[] | undefined): T[] {
  return v === undefined ? [] : Array.isArray(v) ? v : [v];
}

function toError(e: unknown): Error {
  return e instanceof Error ? e : new Error(String(e));
}

/**
 * A `useMutation` that is optimistic, self-reconciling and consistently spoken.
 *
 * Returns the standard react-query mutation result, so `.mutate(vars)`, `.isPending` and the rest
 * behave exactly as call sites already expect.
 */
export function useAppMutation<TData = unknown, TVars = void>(
  opts: AppMutationOptions<TData, TVars>,
) {
  const qc = useQueryClient();
  const {
    mutationFn,
    optimistic: patches,
    invalidates,
    invalidateOnError = true,
    toast,
    onSuccess,
    onError,
  } = opts;

  return useMutation<TData, Error, TVars, Ctx>({
    mutationFn: async (vars) => {
      try {
        return await mutationFn(vars);
      } catch (e) {
        throw toError(e);
      }
    },

    onMutate: async (vars) => {
      const snapshots: Snapshot[] = [];
      for (const spec of asArray(patches)) {
        const key = spec.key(vars);
        if (!key) continue;
        // Stop any in-flight read for this key first: a response that started before our patch
        // would otherwise land after it and silently overwrite the optimistic value.
        await qc.cancelQueries({ queryKey: key });
        const prev = qc.getQueryData(key);
        // Nothing cached yet means nothing to patch *or* to roll back to — the reconcile on settle
        // is what will populate it.
        if (prev === undefined) continue;
        const at = stamp();
        qc.setQueryData(key, spec.patch(prev, vars), { updatedAt: at });
        snapshots.push({ key, prev, stamp: at });
      }
      return { snapshots };
    },

    onError: (cause, vars, ctx) => {
      const err = toError(cause);
      for (const s of ctx?.snapshots ?? []) {
        // Only revert what is still ours. A newer SSE frame or a later mutation on the same key is
        // fresher than our snapshot, and clobbering it back would move the UI backwards.
        if (qc.getQueryState(s.key)?.dataUpdatedAt !== s.stamp) continue;
        qc.setQueryData(s.key, s.prev, { updatedAt: stamp() });
      }
      const spec = toast?.error;
      const text =
        typeof spec === "function" ? spec(err, vars) : spec ? `${spec}: ${err.message}` : undefined;
      if (text) pushToast(text, "error");
      onError?.(err, vars);
    },

    onSuccess: (data, vars) => {
      const spec = toast?.success;
      const text = typeof spec === "function" ? spec(data, vars) : spec;
      if (text) pushToast(text, "success");
      onSuccess?.(data, vars);
    },

    onSettled: (_data, err, vars) => {
      const declared =
        err && !invalidateOnError
          ? []
          : typeof invalidates === "function"
            ? invalidates(vars)
            : (invalidates ?? []);
      // Everything we patched must also be reconciled, or an optimistic value could outlive the
      // truth it was guessing at. Call sites get this without having to remember it.
      const patched = asArray(patches)
        .map((s) => s.key(vars))
        .filter((k): k is QueryKey => k !== undefined);
      const seen = new Set<string>();
      for (const key of [...declared, ...patched]) {
        const id = JSON.stringify(key);
        if (seen.has(id)) continue;
        seen.add(id);
        void qc.invalidateQueries({ queryKey: key });
      }
    },
  });
}

export { toError };
