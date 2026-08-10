import type { PendingFile } from "./capture";

/**
 * Where a capture's *file bytes* survive a reload.
 *
 * [`saveDraft`](./capture.ts) deliberately keeps files out of `localStorage`: a couple of
 * screenshots would blow through its ~5 MB quota and take the recoverable prose down with them.
 * That was the right call about the wrong store. The consequence was a draft that came back from a
 * reload *describing* files it no longer had — the body still said `![](attachment:shot.png)`, the
 * pending list was empty, and creating the task wrote a doc referring to an image that had never
 * been stored. The agent picking that task up days later reads a dangling reference, which is worse
 * than never having pasted the screenshot: it looks like context that exists.
 *
 * IndexedDB is the store that actually fits. It holds tens of megabytes without competing with the
 * prose for quota, so the rule for files becomes the same one the rest of the capture already
 * follows: nothing typed, pasted or dropped is discarded except on a confirmed create.
 *
 * # Every failure here is silent, and that is deliberate
 *
 * Private windows, disabled storage and a full disk all make IndexedDB unavailable, and none of
 * them are a reason to break the form. Every entry point resolves to "no cache" rather than
 * rejecting: the worst case is the behaviour we had before this file existed.
 */

const DB_NAME = "bn-capture";
const DB_VERSION = 1;
const STORE = "pending-files";

/** The single draft's key. There is one new-task form, so there is one cache entry. */
const DRAFT_KEY = "new-task";

/** What one cache entry holds. Keyed rather than a bare array so a second draft could be added. */
interface CacheEntry {
  key: string;
  files: PendingFile[];
}

/** Open the database, or resolve `null` when this browser will not give us one. */
function openDb(): Promise<IDBDatabase | null> {
  return new Promise((resolve) => {
    let request: IDBOpenDBRequest;
    try {
      // Not `window.indexedDB!`: jsdom and private windows really do leave this undefined, and a
      // TypeError thrown out of a draft-persistence path would take the whole dialog with it.
      if (typeof indexedDB === "undefined") return resolve(null);
      request = indexedDB.open(DB_NAME, DB_VERSION);
    } catch {
      return resolve(null);
    }
    request.onupgradeneeded = () => {
      const db = request.result;
      if (!db.objectStoreNames.contains(STORE)) db.createObjectStore(STORE, { keyPath: "key" });
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => resolve(null);
    // A blocked upgrade means another tab holds the old version open. Waiting forever would hang
    // the caller's `await`, so give up and run uncached.
    request.onblocked = () => resolve(null);
  });
}

/**
 * Every access, in call order.
 *
 * IndexedDB orders transactions, but only once they exist — and each call here has to `await` an
 * `open` first, so two calls made in a fixed order can still reach the store in the other one. That
 * matters at exactly one moment: a confirmed create clears the cache and *then* remounts the
 * dialog, whose first act is to read the cache back. Lose that race and the freshly-created task's
 * screenshots reappear in the blank form. A one-slot queue makes the order the caller wrote the
 * order that happens.
 */
let queue: Promise<unknown> = Promise.resolve();

function serial<T>(work: () => Promise<T>): Promise<T> {
  // Both arms are `work`: a previous failure must not stall everything queued behind it.
  const next = queue.then(work, work);
  queue = next.catch(() => undefined);
  return next;
}

/** Run `work` inside a transaction, resolving `fallback` if anything at all goes wrong. */
async function runInStore<T>(
  mode: IDBTransactionMode,
  fallback: T,
  work: (store: IDBObjectStore) => IDBRequest,
): Promise<T> {
  const db = await openDb();
  if (!db) return fallback;
  try {
    return await new Promise<T>((resolve) => {
      const tx = db.transaction(STORE, mode);
      const request = work(tx.objectStore(STORE));
      request.onsuccess = () => resolve((request.result as T) ?? fallback);
      request.onerror = () => resolve(fallback);
      tx.onabort = () => resolve(fallback);
      tx.onerror = () => resolve(fallback);
    });
  } catch {
    return fallback;
  } finally {
    db.close();
  }
}

/** The files cached with the in-progress draft, or an empty list. Never rejects. */
export function loadPendingFiles(): Promise<PendingFile[]> {
  return serial(async () => {
    const entry = await runInStore<CacheEntry | undefined>("readonly", undefined, (store) =>
      store.get(DRAFT_KEY),
    );
    return Array.isArray(entry?.files) ? entry.files : [];
  });
}

/** Mirror the pending files into the cache. An empty list clears the entry rather than storing it. */
export function savePendingFiles(files: PendingFile[]): Promise<void> {
  return serial(async () => {
    await runInStore<unknown>("readwrite", null, (store) =>
      files.length
        ? store.put({ key: DRAFT_KEY, files } satisfies CacheEntry)
        : store.delete(DRAFT_KEY),
    );
  });
}

/** Drop the cached files — called only once a create has actually succeeded. */
export function clearPendingFiles(): Promise<void> {
  return serial(async () => {
    await runInStore<unknown>("readwrite", null, (store) => store.delete(DRAFT_KEY));
  });
}
