import { lazy, Suspense, useCallback, useEffect, useRef, useState } from "react";
import { Paperclip, Plus, X } from "lucide-react";

import type { TaskCreate, TaskLink } from "../api/writes";
import type { CaptureDraft, PendingFile } from "../lib/capture";
import {
  draftIsEmpty,
  extractUrls,
  fileToPending,
  humanSize,
  inlineAttachmentNames,
  isImage,
  loadDraft,
  looksLikeIssueUrl,
  MAX_FILE_BYTES,
  saveDraft,
  toUploads,
} from "../lib/capture";
import { loadPendingFiles, savePendingFiles } from "../lib/captureFiles";
import { Button } from "./ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "./ui/dialog";
import { Input } from "./ui/input";
import { Label } from "./ui/label";
import { RepoSelect } from "./RepoSelect";

const CaptureEditorBody = lazy(() => import("./CaptureEditorBody"));

/**
 * The only way to originate work by hand (#1006).
 *
 * It replaces **Start an agent**, and the difference is the point: that spawned a process from a
 * free-text prompt and left nothing behind, so the intent vanished with the agent. This writes a
 * durable record first, and an agent picks it up afterwards.
 *
 * Only the title is required. Everything else is what the SMART gate (#998) will ask for anyway —
 * offering the fields here saves a round trip through the enhance agent, but a bare title is a
 * legitimate capture: better a rough task written down than a good one lost while you look up the
 * estimate.
 *
 * # Nothing typed here is ever discarded
 *
 * This form used to blank every field the instant it called `onSubmit`, before knowing whether the
 * create had succeeded. A rejected estimate or a dropped connection therefore left the dialog open
 * and empty, and what the operator had typed was gone. Now the draft is cleared **only** on a
 * confirmed success, and it is mirrored out of memory on the way — the text into `localStorage`,
 * the file bytes into IndexedDB ([`captureFiles`](../lib/captureFiles.ts)) — so a reload or a
 * closed tab does not take it either. Everything below follows from that: the richer the capture
 * gets, the more expensive losing it is.
 */
export function NewTaskDialog({
  open,
  busy,
  onOpenChange,
  onSubmit,
}: {
  open: boolean;
  busy?: boolean;
  onOpenChange: (open: boolean) => void;
  onSubmit: (body: TaskCreate) => void;
}) {
  const [draft, setDraft] = useState<CaptureDraft>(() => loadDraft());
  // File bytes are too big for `localStorage` (see `saveDraft`), so they are mirrored into
  // IndexedDB instead — asynchronously, which is why they start empty and arrive below.
  const [files, setFiles] = useState<PendingFile[]>([]);
  const [linkDraft, setLinkDraft] = useState("");
  const [fileError, setFileError] = useState<string | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);

  const set = useCallback(<K extends keyof CaptureDraft>(key: K, value: CaptureDraft[K]) => {
    setDraft((prev) => ({ ...prev, [key]: value }));
  }, []);

  // Mirror to storage on every change. Cheap next to what it protects, and it means the recovery
  // point is "whatever you last typed" rather than "whenever we last remembered to save".
  useEffect(() => saveDraft(draft), [draft]);

  // The editor's `uploadFile` callback and the drop/paste handlers all need to see the files added
  // by the others, and reading state inside an async callback would see whatever was current when
  // it was created. A ref is the only copy that is always up to date.
  const filesRef = useRef<PendingFile[]>(files);
  filesRef.current = files;

  // Read the cached bytes back before mirroring anything out, so the empty initial state cannot
  // overwrite a real cache in the window before the read lands. Anything pasted while the read is
  // in flight is kept: the restored files go *behind* it rather than replacing the list.
  const restored = useRef(false);
  useEffect(() => {
    let live = true;
    void loadPendingFiles().then((cached) => {
      if (!live) return;
      if (cached.length) {
        const taken = new Set(filesRef.current.map((f) => f.name));
        setFiles([...cached.filter((f) => !taken.has(f.name)), ...filesRef.current]);
      }
      restored.current = true;
    });
    return () => {
      live = false;
    };
  }, []);

  useEffect(() => {
    if (!restored.current) return;
    void savePendingFiles(files);
  }, [files]);

  const addFiles = useCallback(async (incoming: File[]) => {
    const tooBig = incoming.find((f) => f.size > MAX_FILE_BYTES);
    if (tooBig) {
      setFileError(`${tooBig.name} is over ${humanSize(MAX_FILE_BYTES)} — link to it instead`);
    }
    const ok = incoming.filter((f) => f.size <= MAX_FILE_BYTES);
    if (!ok.length) return;
    // Read sequentially so each name is de-duplicated against the ones before it, including the
    // ones this same call is adding — two screenshots pasted together are both `image.png`.
    const taken = new Set(filesRef.current.map((f) => f.name));
    const read: PendingFile[] = [];
    for (const file of ok) {
      const stored = await fileToPending(file, taken);
      taken.add(stored.name);
      read.push(stored);
    }
    setFiles([...filesRef.current, ...read]);
  }, []);

  const addLink = useCallback((raw: string) => {
    const url = raw.trim();
    if (!url) return;
    setDraft((prev) => {
      if (prev.links.some((l) => l.url === url)) return prev;
      const links: TaskLink[] = [...prev.links, { url }];
      // An issue or PR URL is the task's provenance, and `task_agents::pre_score` scores a task
      // with one higher. Filling `src` from the first such link saves asking for it twice.
      const src = prev.src || (looksLikeIssueUrl(url) ? url : "");
      return { ...prev, links, src };
    });
    setLinkDraft("");
  }, []);

  /**
   * Paste is the fast path: a screenshot becomes an attachment, a copied URL becomes a link.
   *
   * This is on the dialog, not the editor, so a paste anywhere in the form is caught — including
   * with the cursor in the title field, which is where it usually is. The cost of catching
   * everything is that it also sees the pastes the editor has *already* handled: BlockNote's
   * ProseMirror plugin calls `preventDefault` and routes files through `uploadFile`, but it does
   * not stop propagation, so the event still bubbles up to here. Without the `defaultPrevented`
   * check a screenshot pasted into the editor was stored twice — once as the image block, once
   * again as `image-2.png` — and both copies were uploaded.
   */
  const onPaste = useCallback(
    (e: React.ClipboardEvent) => {
      if (e.defaultPrevented) return;
      const pasted = Array.from(e.clipboardData.files);
      if (pasted.length) {
        e.preventDefault();
        void addFiles(pasted);
        return;
      }
      const text = e.clipboardData.getData("text");
      if (text && extractUrls(text).length === 1 && text.trim() === extractUrls(text)[0]) {
        e.preventDefault();
        addLink(text);
      }
    },
    [addFiles, addLink],
  );

  const submit = () => {
    if (!draft.title.trim()) return;
    onSubmit({
      title: draft.title.trim(),
      repo: draft.repo.trim() || undefined,
      day: draft.day.trim() || undefined,
      est: draft.est.trim() || undefined,
      priority: draft.priority.trim() || undefined,
      src: draft.src.trim() || undefined,
      // The editor addresses pending files by an opaque URL; the doc knows them by name.
      body: inlineAttachmentNames(draft.body).trim() || undefined,
      links: draft.links.length ? draft.links : undefined,
      attachments: files.length ? toUploads(files) : undefined,
    });
    // Deliberately no reset here. The parent remounts this dialog once the server has confirmed
    // the create — until then every keystroke is still the only copy that exists.
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        className="max-w-2xl"
        onPaste={onPaste}
        onDragOver={(e) => e.preventDefault()}
        // Same bubbling story as `onPaste`: a file dropped *on the editor* is already BlockNote's,
        // and re-adding it here would store it twice.
        onDrop={(e) => {
          if (e.defaultPrevented) return;
          const dropped = Array.from(e.dataTransfer.files);
          if (!dropped.length) return;
          e.preventDefault();
          void addFiles(dropped);
        }}
      >
        <DialogHeader>
          <DialogTitle>New task</DialogTitle>
          <DialogDescription>
            A title is enough. Anything else you write, paste or drop lands in the task&apos;s
            context doc — the enhance agent fills in the rest before an agent may execute it.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-3">
          <div className="space-y-1.5">
            <Label htmlFor="task-title" className="text-[11px]">
              What changes, and where
            </Label>
            <Input
              id="task-title"
              autoFocus
              value={draft.title}
              onChange={(e) => set("title", e.target.value)}
              // ⌘/Ctrl+Enter submits from the title field so a bare capture never needs the mouse.
              onKeyDown={(e) => {
                if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) submit();
              }}
              placeholder="Fix the SSE reconnect storm on flaky LAN"
            />
          </div>

          <div className="space-y-1.5">
            <Label className="text-[11px]">
              Context{" "}
              <span className="font-normal text-muted-foreground">
                — paste screenshots and logs straight in
              </span>
            </Label>
            <Suspense
              fallback={
                <div className="rounded-md border border-border bg-muted/40 px-3 py-6 text-center text-xs text-muted-foreground">
                  loading the editor…
                </div>
              }
            >
              <CaptureEditorBody
                initialMarkdown={draft.body}
                onChange={(markdown) => set("body", markdown)}
                onFiles={setFiles}
                files={files}
              />
            </Suspense>
          </div>

          {(files.length > 0 || fileError) && (
            <div className="space-y-1.5">
              <Label className="text-[11px]">Attachments</Label>
              {fileError && <p className="text-[11px] text-destructive">{fileError}</p>}
              <ul className="flex flex-wrap gap-2">
                {files.map((file) => (
                  <li
                    key={file.name}
                    className="flex items-center gap-2 rounded-md border border-border bg-muted/40 py-1 pl-1 pr-2"
                  >
                    {isImage(file) ? (
                      <img
                        src={file.dataUrl}
                        alt={file.name}
                        className="h-8 w-8 rounded object-cover"
                      />
                    ) : (
                      <span className="flex h-8 w-8 items-center justify-center rounded bg-background">
                        <Paperclip className="h-3.5 w-3.5 text-muted-foreground" />
                      </span>
                    )}
                    <span className="max-w-[12rem] truncate font-mono text-[11px]">
                      {file.name}
                    </span>
                    <span className="text-[10px] text-muted-foreground">
                      {humanSize(file.size)}
                    </span>
                    <button
                      type="button"
                      aria-label={`Remove ${file.name}`}
                      className="text-muted-foreground hover:text-foreground"
                      onClick={() => setFiles((prev) => prev.filter((f) => f.name !== file.name))}
                    >
                      <X className="h-3 w-3" />
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          )}

          <div className="space-y-1.5">
            <Label htmlFor="task-link" className="text-[11px]">
              Links{" "}
              <span className="font-normal text-muted-foreground">— the issue, the thread</span>
            </Label>
            <div className="flex gap-2">
              <Input
                id="task-link"
                value={linkDraft}
                onChange={(e) => setLinkDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    addLink(linkDraft);
                  }
                }}
                placeholder="https://github.com/owner/repo/issues/42"
              />
              <Button
                type="button"
                size="sm"
                variant="secondary"
                disabled={!linkDraft.trim()}
                onClick={() => addLink(linkDraft)}
              >
                <Plus /> Add
              </Button>
            </div>
            {draft.links.length > 0 && (
              <ul className="space-y-1">
                {draft.links.map((link) => (
                  <li key={link.url} className="flex items-center gap-2 text-[11px]">
                    <span className="truncate font-mono text-muted-foreground">{link.url}</span>
                    <button
                      type="button"
                      aria-label={`Remove ${link.url}`}
                      className="text-muted-foreground hover:text-foreground"
                      onClick={() =>
                        set(
                          "links",
                          draft.links.filter((l) => l.url !== link.url),
                        )
                      }
                    >
                      <X className="h-3 w-3" />
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </div>

          <div className="grid grid-cols-4 gap-2">
            <div className="space-y-1.5">
              <Label className="text-[11px]">Repo</Label>
              <RepoSelect value={draft.repo} onChange={(v) => set("repo", v)} />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="task-day" className="text-[11px]">
                Day
              </Label>
              <Input
                id="task-day"
                value={draft.day}
                onChange={(e) => set("day", e.target.value)}
                placeholder="2026-08-09"
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="task-est" className="text-[11px]">
                Estimate
              </Label>
              <Input
                id="task-est"
                value={draft.est}
                onChange={(e) => set("est", e.target.value)}
                placeholder="2h"
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="task-priority" className="text-[11px]">
                Priority
              </Label>
              <Input
                id="task-priority"
                value={draft.priority}
                onChange={(e) => set("priority", e.target.value.toUpperCase().slice(0, 1))}
                placeholder="A"
              />
            </div>
          </div>
        </div>

        <input
          ref={fileInput}
          type="file"
          multiple
          className="hidden"
          onChange={(e) => {
            void addFiles(Array.from(e.target.files ?? []));
            e.target.value = "";
          }}
        />

        <DialogFooter className="sm:justify-between">
          <Button
            type="button"
            size="sm"
            variant="ghost"
            onClick={() => fileInput.current?.click()}
          >
            <Paperclip /> Attach
          </Button>
          <div className="flex gap-2">
            <Button variant="secondary" size="sm" onClick={() => onOpenChange(false)}>
              {draftIsEmpty(draft) && files.length === 0 ? "Cancel" : "Keep for later"}
            </Button>
            <Button size="sm" disabled={busy || !draft.title.trim()} onClick={submit}>
              Create
            </Button>
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
