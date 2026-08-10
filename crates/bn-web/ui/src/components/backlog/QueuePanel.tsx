import { useState } from "react";
import { X } from "lucide-react";

import type { RunView } from "../../api/types";
import { moveId, queuedItems } from "../../lib/backlog";
import { cn } from "../../lib/utils";
import { Badge } from "../ui/badge";
import { Button } from "../ui/button";

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
    <div className="mb-3 rounded-md border border-border bg-card p-4">
      <div className="mb-3 flex flex-wrap items-center gap-2">
        <h4 className="text-sm font-semibold">Queued</h4>
        <Badge variant="muted" className="tabular-nums">
          {queued.length}
        </Badge>
        {queued.length > 1 && (
          <span className="text-xs text-muted-foreground">drag to reprioritize</span>
        )}
      </div>

      {queued.length === 0 ? (
        <p className="py-2 text-sm text-muted-foreground">No queued items.</p>
      ) : (
        <ul className="flex flex-col gap-2">
          {queued.map((it, idx) => (
            <li
              key={it.id}
              className={cn(
                "flex items-center gap-2 rounded-sm border border-border bg-background px-3 py-1.5",
                dragId === it.id && "opacity-50",
                overId === it.id && dragId !== it.id && "ring-1 ring-ring",
              )}
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
              <span className="cursor-grab font-mono text-muted-foreground" aria-hidden="true">
                ⠿
              </span>
              <span className="min-w-[22px] font-mono text-sm tabular-nums text-muted-foreground">
                {idx + 1}
              </span>
              <span className="min-w-0 flex-1 truncate font-mono text-sm" title={it.item}>
                {it.item}
              </span>
              <Button
                type="button"
                variant="ghost"
                size="icon"
                className="h-8 w-8 text-muted-foreground hover:text-urgent"
                onClick={() => onRemove(it.id)}
                disabled={busy}
                title="remove from queue"
                aria-label={`remove ${it.item}`}
              >
                <X />
              </Button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
