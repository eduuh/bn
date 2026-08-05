import { useEffect, useState, useSyncExternalStore } from "react";
import {
  subscribe,
  getToasts,
  dismissToast,
  mountStack,
  unmountStack,
  ownerStackId,
  subscribeOwner,
} from "../lib/toast";
import { cn } from "../lib/utils";

/** Toast tone → accent border/text tokens; `info` keeps the neutral surface. */
const toastTone: Record<string, string> = {
  success: "border-ok/60",
  error: "border-urgent/60 text-[#ffb4ab]",
  info: "",
};

/** The fixed toast overlay. Reads the provider-free `lib/toast` store directly. Safe to mount more
 * than once (many pages do, and the app shell mounts one for the command palette's composer): a
 * module-level owner guard lets only the first-mounted instance actually render, so toasts never
 * duplicate. Click a toast to dismiss it. */
export function ToastStack() {
  const toasts = useSyncExternalStore(subscribe, getToasts, getToasts);
  const ownerId = useSyncExternalStore(subscribeOwner, ownerStackId, ownerStackId);
  const [id, setId] = useState(0);

  useEffect(() => {
    const myId = mountStack();
    setId(myId);
    return () => unmountStack(myId);
  }, []);

  // Only the owning (first-mounted) instance renders; the rest stay invisible but keep their
  // registration so ownership can transfer when the owner unmounts.
  if (id === 0 || ownerId !== id || toasts.length === 0) return null;
  return (
    <div
      className="fixed bottom-[calc(58px+env(safe-area-inset-bottom)+0.75rem)] left-1/2 z-50 flex w-[min(92vw,520px)] -translate-x-1/2 flex-col gap-2 md:bottom-5 md:left-auto md:right-5 md:translate-x-0"
      aria-live="polite"
    >
      {toasts.map((t) => (
        <button
          key={t.id}
          type="button"
          className={cn(
            "flex items-center gap-2 rounded-lg border border-input bg-secondary px-4 py-3 text-left text-sm shadow-pop",
            toastTone[t.tone] ?? "",
          )}
          onClick={() => dismissToast(t.id)}
          title="dismiss"
        >
          {t.message}
        </button>
      ))}
    </div>
  );
}
