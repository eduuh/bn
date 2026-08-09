import { useCallback, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Download, Paperclip, Plus, X } from "lucide-react";

import { queryKeys } from "../api/client";
import type { TaskAttachmentUpload } from "../api/writes";
import {
  addTaskAttachments,
  deleteTaskAttachment,
  taskAttachmentUrl,
  taskAttachments,
} from "../api/writes";
import { useAppMutation } from "../hooks/useAppMutation";
import { fileToPending, humanSize, MAX_FILE_BYTES, toUploads } from "../lib/capture";
import { Button } from "./ui/button";
import { Skeleton } from "./ui/skeleton";

/**
 * The files captured with a task — added at capture time, or dropped in later once the screenshot
 * finally reproduces.
 *
 * Its own query rather than a field on the task detail: an attachment list is a directory scan,
 * and the detail view polls. The server keeps the doc's `## Attachments` manifest in step with this
 * directory on every write, so what an agent reads and what this shows cannot drift.
 */
export function TaskAttachments({ taskId }: { taskId: string }) {
  const [error, setError] = useState<string | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const key = [queryKeys.taskAttachments(taskId)];

  const q = useQuery({
    queryKey: key,
    queryFn: () => taskAttachments(taskId),
  });

  const add = useAppMutation({
    mutationFn: (uploads: TaskAttachmentUpload[]) => addTaskAttachments(taskId, uploads),
    invalidates: key,
    toast: {
      success: (r) => `Attached ${r.attachments.length} file(s)`,
      error: "Could not attach",
    },
  });
  const drop = useAppMutation({
    mutationFn: (name: string) => deleteTaskAttachment(taskId, name),
    invalidates: key,
    toast: { success: "Removed", error: "Could not remove" },
  });

  const onPick = useCallback(
    async (picked: File[]) => {
      setError(null);
      const tooBig = picked.find((f) => f.size > MAX_FILE_BYTES);
      if (tooBig) {
        setError(`${tooBig.name} is over ${humanSize(MAX_FILE_BYTES)} — link to it instead`);
        return;
      }
      if (!picked.length) return;
      const taken = new Set((q.data?.attachments ?? []).map((a) => a.name));
      const pending = [];
      for (const file of picked) {
        const stored = await fileToPending(file, taken);
        taken.add(stored.name);
        pending.push(stored);
      }
      add.mutate(toUploads(pending));
    },
    [add, q.data],
  );

  const files = q.data?.attachments ?? [];

  return (
    <div
      className="space-y-2"
      onDragOver={(e) => e.preventDefault()}
      onDrop={(e) => {
        const dropped = Array.from(e.dataTransfer.files);
        if (!dropped.length) return;
        e.preventDefault();
        void onPick(dropped);
      }}
      onPaste={(e) => {
        const pasted = Array.from(e.clipboardData.files);
        if (!pasted.length) return;
        e.preventDefault();
        void onPick(pasted);
      }}
    >
      {q.isLoading ? (
        <Skeleton className="h-16 w-full" />
      ) : files.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          Nothing attached. Drop a screenshot or a log here — the agent working this task reads them
          by name from <code className="font-mono">bn task attach {taskId}</code>.
        </p>
      ) : (
        <ul className="flex flex-wrap gap-2">
          {files.map((file) => (
            <li
              key={file.name}
              className="w-40 space-y-1 rounded-md border border-border bg-card p-2"
            >
              {file.is_image ? (
                <a href={taskAttachmentUrl(taskId, file.name)} target="_blank" rel="noreferrer">
                  <img
                    src={taskAttachmentUrl(taskId, file.name)}
                    alt={file.name}
                    className="h-20 w-full rounded object-cover"
                  />
                </a>
              ) : (
                <div className="flex h-20 items-center justify-center rounded bg-muted/50">
                  <Paperclip className="h-5 w-5 text-muted-foreground" />
                </div>
              )}
              <div className="truncate font-mono text-[11px]" title={file.name}>
                {file.name}
              </div>
              <div className="flex items-center justify-between text-[10px] text-muted-foreground">
                <span>{file.human_size}</span>
                <span className="flex items-center gap-1">
                  <a
                    href={taskAttachmentUrl(taskId, file.name)}
                    download={file.name}
                    aria-label={`Download ${file.name}`}
                    className="hover:text-foreground"
                  >
                    <Download className="h-3 w-3" />
                  </a>
                  <button
                    type="button"
                    aria-label={`Remove ${file.name}`}
                    disabled={drop.isPending}
                    className="hover:text-foreground"
                    onClick={() => drop.mutate(file.name)}
                  >
                    <X className="h-3 w-3" />
                  </button>
                </span>
              </div>
            </li>
          ))}
        </ul>
      )}

      {error && <p className="text-[11px] text-destructive">{error}</p>}

      <input
        ref={fileInput}
        type="file"
        multiple
        className="hidden"
        onChange={(e) => {
          void onPick(Array.from(e.target.files ?? []));
          e.target.value = "";
        }}
      />
      <Button
        type="button"
        size="sm"
        variant="secondary"
        disabled={add.isPending}
        onClick={() => fileInput.current?.click()}
      >
        <Plus /> Attach a file
      </Button>
    </div>
  );
}
