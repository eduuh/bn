// A tiny, provider-free toast store. Pages subscribe with `useToasts` and fire `pushToast` from
// mutation callbacks (design §4(c): "reflect results via … toasts"). Module-level so any component
// can raise one without threading a context; a single `<ToastStack>` per page renders them.

export type ToastTone = "success" | "error" | "info";

export interface Toast {
  id: number;
  message: string;
  tone: ToastTone;
}

let toasts: Toast[] = [];
let nextId = 1;
const listeners = new Set<() => void>();

function emit(): void {
  for (const l of listeners) l();
}

/** Raise a toast; it auto-dismisses after ~4.5s (click to dismiss sooner). */
export function pushToast(message: string, tone: ToastTone = "info"): void {
  const id = nextId++;
  toasts = [...toasts, { id, message, tone }];
  emit();
  window.setTimeout(() => dismissToast(id), 4500);
}

/** Remove a toast by id. */
export function dismissToast(id: number): void {
  toasts = toasts.filter((t) => t.id !== id);
  emit();
}

/** `useSyncExternalStore` subscribe: returns an unsubscribe. */
export function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** The current toast list (a stable reference between mutations, for `useSyncExternalStore`). */
export function getToasts(): Toast[] {
  return toasts;
}

// --- Single-renderer guard --------------------------------------------------
// Many pages mount their own `<ToastStack>`, and the app shell mounts one too (so the app-wide
// command palette's `StartAgentComposer` can toast from any route). The store above is
// module-level, so every mounted stack would otherwise render every toast. This registry lets only
// the first-mounted instance render; ownership transfers on unmount, so there's always exactly one
// visible overlay regardless of how many `<ToastStack>`s are mounted.

const mountedStacks: number[] = [];
let stackSeq = 0;
const ownerListeners = new Set<() => void>();

function emitOwner(): void {
  for (const l of ownerListeners) l();
}

/** Register a mounted stack; returns its id. Call from a mount effect. */
export function mountStack(): number {
  const id = ++stackSeq;
  mountedStacks.push(id);
  emitOwner();
  return id;
}

/** Deregister a stack on unmount, transferring ownership to the next-mounted one. */
export function unmountStack(id: number): void {
  const i = mountedStacks.indexOf(id);
  if (i >= 0) mountedStacks.splice(i, 1);
  emitOwner();
}

/** The id of the stack currently allowed to render (0 = none mounted). */
export function ownerStackId(): number {
  return mountedStacks[0] ?? 0;
}

/** `useSyncExternalStore` subscribe for the owner id. */
export function subscribeOwner(listener: () => void): () => void {
  ownerListeners.add(listener);
  return () => ownerListeners.delete(listener);
}
