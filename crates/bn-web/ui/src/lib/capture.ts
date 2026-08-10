import type { TaskAttachmentUpload, TaskLink } from "../api/writes";

/**
 * Capture-time state: the files, links and prose recorded with a task before it exists.
 *
 * The theme here is that **nothing typed is ever thrown away by us**. A capture is the one moment
 * where the work exists only in the operator's head and in this form — there is no server copy to
 * fall back on — so every path that could discard it (a failed create, a reload, a closed tab) has
 * to be one we deliberately handled.
 */

/** The opaque URL scheme a not-yet-uploaded file is addressed by inside the editor. */
export const ATTACHMENT_SCHEME = "attachment:";

/** The largest file the capture form accepts, mirroring `bn_core::task_attach::MAX_BYTES`. */
export const MAX_FILE_BYTES = 10 * 1024 * 1024;

/** A file held in the browser, waiting for the task it belongs to to exist. */
export interface PendingFile {
  /** The de-duplicated name it will be stored under. */
  name: string;
  /** Base64 with the `data:` prefix — directly usable as an `<img src>` for the preview. */
  dataUrl: string;
  size: number;
  type: string;
}

/** Whether this is something the dialog can preview inline. */
export function isImage(file: PendingFile): boolean {
  return file.type.startsWith("image/");
}

/** A whole-KB/MB size, matching what the server renders into the doc's manifest. */
export function humanSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/**
 * A name not already in `taken`: `shot.png`, then `shot-2.png`, …
 *
 * The server de-duplicates too, and authoritatively. Doing it here as well is what keeps the
 * editor's `attachment:` references pointing at the right file when two screenshots are pasted in
 * a row — the second one's name has to be settled *before* it goes into the markdown.
 */
export function uniqueName(name: string, taken: Set<string>): string {
  if (!taken.has(name)) return name;
  const dot = name.lastIndexOf(".");
  const stem = dot > 0 ? name.slice(0, dot) : name;
  const ext = dot > 0 ? name.slice(dot) : "";
  for (let n = 2; n < 1000; n++) {
    const candidate = `${stem}-${n}${ext}`;
    if (!taken.has(candidate)) return candidate;
  }
  return name;
}

/** Read a `File` into a [`PendingFile`], under a name that does not collide with `taken`. */
export async function fileToPending(file: File, taken: Set<string>): Promise<PendingFile> {
  const dataUrl = await new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    // `readAsDataURL` always yields a string; the union is `FileReader`'s, shared with the
    // ArrayBuffer readers. Narrowing rather than stringifying keeps a surprise from becoming
    // "[object Object]" pretending to be a file.
    reader.onload = () =>
      typeof reader.result === "string"
        ? resolve(reader.result)
        : reject(new Error("could not read the file as a data URL"));
    reader.onerror = () => reject(reader.error ?? new Error("could not read the file"));
    reader.readAsDataURL(file);
  });
  return {
    name: uniqueName(file.name || "pasted-file", taken),
    dataUrl,
    size: file.size,
    type: file.type || "application/octet-stream",
  };
}

/** Turn pending files into what `POST /api/tasks` takes. */
export function toUploads(files: PendingFile[]): TaskAttachmentUpload[] {
  return files.map((f) => ({ name: f.name, data: f.dataUrl }));
}

/**
 * Rewrite the editor's `attachment:` URLs down to bare file names.
 *
 * A name is the identifier an attachment has everywhere that outlives this form — the doc's
 * `## Attachments` manifest, the `/api/tasks/:id/attach/:name` path, and `bn task attach`. A URL
 * would be a lie in the document: it is resolvable only in this browser, in this session, before
 * the task even exists.
 */
export function inlineAttachmentNames(markdown: string): string {
  // `split`/`join` rather than `replaceAll`: the build targets a lib older than ES2021.
  return markdown.split(`](${ATTACHMENT_SCHEME}`).join("](");
}

/** The draft as it is persisted — everything except the file bytes. */
export interface CaptureDraft {
  title: string;
  body: string;
  repo: string;
  day: string;
  est: string;
  priority: string;
  src: string;
  links: TaskLink[];
}

export const EMPTY_DRAFT: CaptureDraft = {
  title: "",
  body: "",
  repo: "",
  day: "",
  est: "",
  priority: "",
  src: "",
  links: [],
};

const DRAFT_KEY = "bn:new-task-draft";

/** Whether anything has actually been typed — an all-blank draft is not worth restoring. */
export function draftIsEmpty(draft: CaptureDraft): boolean {
  return (
    !draft.title.trim() &&
    !draft.body.trim() &&
    !draft.repo.trim() &&
    !draft.day.trim() &&
    !draft.est.trim() &&
    !draft.priority.trim() &&
    !draft.src.trim() &&
    draft.links.length === 0
  );
}

/**
 * Persist the in-progress capture so a reload, a crashed tab or a mis-click does not erase it.
 *
 * File bytes are **not** persisted here: a few screenshots would blow through the ~5 MB
 * `localStorage` quota and take the recoverable text down with them. They go to IndexedDB instead
 * — see [`captureFiles`](./captureFiles.ts), which exists because a restored draft whose files had
 * been dropped still *referenced* them, and created a task pointing at images it never had.
 */
export function saveDraft(draft: CaptureDraft): void {
  try {
    if (draftIsEmpty(draft)) {
      window.localStorage.removeItem(DRAFT_KEY);
      return;
    }
    window.localStorage.setItem(DRAFT_KEY, JSON.stringify(draft));
  } catch {
    /* private mode, or a full quota — a draft that cannot be saved must not break the form */
  }
}

/** The persisted draft, or a blank one. Never throws: a corrupt entry is just a missing draft. */
export function loadDraft(): CaptureDraft {
  try {
    const raw = window.localStorage.getItem(DRAFT_KEY);
    if (!raw) return { ...EMPTY_DRAFT };
    const parsed = JSON.parse(raw) as Partial<CaptureDraft>;
    return {
      ...EMPTY_DRAFT,
      ...parsed,
      links: Array.isArray(parsed.links) ? parsed.links : [],
    };
  } catch {
    return { ...EMPTY_DRAFT };
  }
}

/** Drop the persisted draft — called only once a create has actually succeeded. */
export function clearDraft(): void {
  try {
    window.localStorage.removeItem(DRAFT_KEY);
  } catch {
    /* nothing to do: the draft is already unreachable */
  }
}

/**
 * Resolve a task doc's image sources against that task's attachment endpoint.
 *
 * The doc refers to captured files by bare name, which is right for a document that syncs across
 * machines and moves on pickup, and meaningless to an `<img>` — the browser would resolve
 * `shot.png` against `/tasks/t42` and 404. Anything already rooted or carrying a scheme is left
 * alone: a doc may legitimately embed an external image, and rewriting that would break it.
 */
export function attachmentResolver(taskId: string): (src: string) => string {
  return (src) => {
    const name = src.trim();
    if (!name || name.startsWith("/") || /^[a-z][a-z0-9+.-]*:/i.test(name)) return src;
    // A bare file name is an attachment; a path with segments is not something bn writes.
    if (name.includes("/")) return src;
    return `/api/tasks/${encodeURIComponent(taskId)}/attach/${encodeURIComponent(name)}`;
  };
}

/** A GitHub/GitLab issue or PR URL, which is worth recording as the task's `src:` provenance. */
export function looksLikeIssueUrl(url: string): boolean {
  return /^https?:\/\/[^\s]+\/(issues|pull|merge_requests|-\/issues)\/\d+/i.test(url.trim());
}

/** Every http(s) URL in a blob of text, de-duplicated in first-seen order. */
export function extractUrls(text: string): string[] {
  const found = text.match(/https?:\/\/[^\s<>()[\]"']+/g) ?? [];
  return [...new Set(found.map((u) => u.replace(/[.,;:]+$/, "")))];
}
