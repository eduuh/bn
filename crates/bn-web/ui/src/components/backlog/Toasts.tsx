import type { Toast } from "../../hooks/useToasts";

/** The fixed toast stack (bottom-right). Each toast is click-to-dismiss and auto-expires; `ok`
 * greens, `error` reds — the write-result surface the design §4 optimistic rule calls for. */
export function Toasts({
  toasts,
  onDismiss,
}: {
  toasts: Toast[];
  onDismiss: (id: number) => void;
}) {
  if (toasts.length === 0) return null;
  return (
    <div className="toasts" role="status" aria-live="polite">
      {toasts.map((t) => (
        <button
          key={t.id}
          type="button"
          className={`toast toast-${t.kind}`}
          onClick={() => onDismiss(t.id)}
          title="dismiss"
        >
          {t.text}
        </button>
      ))}
    </div>
  );
}
