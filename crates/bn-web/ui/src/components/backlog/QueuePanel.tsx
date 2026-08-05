import { useState } from "react";
import type { RunView } from "../../api/types";
import { moveId, queuedItems } from "../../lib/backlog";

/**
 * The queued list (design §4(d)): native HTML5 drag-reorder → `POST queue/reorder` (optimistically
 * patched, reconciled by the SSE frame) and a per-item remove → `DELETE queue/:id`. Only *queued*
 * items appear — in-flight items are killed from the board / detail views, never removed here.
 */
export function QueuePanel({
  run,
  busy,
  onReorder,
  onRemove,
}: {
  run: RunView;
  busy: boolean;
  onReorder: (ids: string[]) => void;
  onRemove: (id: string) => void;
}) {
  const queued = queuedItems(run);
  const [dragId, setDragId] = useState<string | null>(null);
  const [overId, setOverId] = useState<string | null>(null);

  const drop = (targetId: string) => {
    if (busy || !dragId || dragId === targetId) return;
    const ids = queued.map((i) => i.id);
    const from = ids.indexOf(dragId);
    const to = ids.indexOf(targetId);
    const next = moveId(ids, from, to);
    if (next !== ids) onReorder(next);
  };

  return (
    <div className="queue-panel">
      <div className="queue-panel-head">
        <h4>Queued</h4>
        <span className="count">{queued.length}</span>
        {queued.length > 1 && <span className="sub">drag to reprioritize</span>}
      </div>

      {queued.length === 0 ? (
        <p className="empty">No queued items.</p>
      ) : (
        <ul className="queue-reorder">
          {queued.map((it, idx) => (
            <li
              key={it.id}
              className={`qr-row${dragId === it.id ? " dragging" : ""}${
                overId === it.id && dragId !== it.id ? " drag-over" : ""
              }`}
              draggable={!busy}
              onDragStart={(e) => {
                setDragId(it.id);
                e.dataTransfer.effectAllowed = "move";
                e.dataTransfer.setData("text/plain", it.id);
              }}
              onDragOver={(e) => {
                e.preventDefault();
                e.dataTransfer.dropEffect = "move";
                if (overId !== it.id) setOverId(it.id);
              }}
              onDrop={(e) => {
                e.preventDefault();
                drop(it.id);
                setDragId(null);
                setOverId(null);
              }}
              onDragEnd={() => {
                setDragId(null);
                setOverId(null);
              }}
            >
              <span className="qr-handle mono" aria-hidden="true">
                ⠿
              </span>
              <span className="qr-pos mono">{idx + 1}</span>
              <span className="qr-item mono" title={it.item}>
                {it.item}
              </span>
              <button
                type="button"
                className="qr-remove"
                onClick={() => onRemove(it.id)}
                disabled={busy}
                title="remove from queue"
                aria-label={`remove ${it.item}`}
              >
                ✕
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
