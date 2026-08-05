import { useCallback, useRef, useState } from "react";

export type ToastKind = "ok" | "error";
export interface Toast {
  id: number;
  kind: ToastKind;
  text: string;
}

/** A tiny transient-toast store: `push` adds one (auto-dismissed after `ttl`ms), `dismiss` drops
 * one early. Used by the backlog page to surface a write's success / `!ok` failure (design §4). */
export function useToasts(ttl = 4000) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const seq = useRef(0);

  const dismiss = useCallback((id: number) => {
    setToasts((t) => t.filter((x) => x.id !== id));
  }, []);

  const push = useCallback(
    (kind: ToastKind, text: string) => {
      const id = ++seq.current;
      setToasts((t) => [...t, { id, kind, text }]);
      window.setTimeout(() => dismiss(id), ttl);
    },
    [dismiss, ttl],
  );

  return { toasts, push, dismiss };
}
