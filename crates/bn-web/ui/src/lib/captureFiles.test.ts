import { afterEach, describe, expect, it } from "vitest";

import type { PendingFile } from "./capture";
import { clearPendingFiles, loadPendingFiles, savePendingFiles } from "./captureFiles";

/**
 * The pending-file cache, exercised against a hand-rolled IndexedDB stand-in.
 *
 * jsdom ships no IndexedDB, which makes the *absence* path the default here — worth testing on its
 * own, since a private window hits exactly that and the form has to keep working. For the rest,
 * the stub below implements only the four calls `captureFiles` makes (`open`, `get`, `put`,
 * `delete`) and dispatches their callbacks asynchronously, which is the part that matters: the
 * ordering guarantee under test is only interesting because these are not synchronous.
 */

const file = (name: string): PendingFile => ({
  name,
  dataUrl: `data:image/png;base64,${name}`,
  size: 3,
  type: "image/png",
});

/** Install a minimal async IndexedDB over one in-memory record store. Returns the uninstaller. */
function stubIndexedDb() {
  const rows = new Map<string, unknown>();
  // The callbacks are assigned *after* the request object is returned, so every completion has to
  // wait at least a turn — the same shape as the real thing, and the reason ordering can go wrong.
  const request = <T>(result: () => T, delay = 0) => {
    const req: Record<string, unknown> = { result: undefined };
    setTimeout(() => {
      req.result = result();
      (req.onsuccess as (() => void) | undefined)?.();
    }, delay);
    return req;
  };
  // Each `open` resolves sooner than the one before it, so a later call reaches the store first
  // unless something is holding the order. A uniform delay would make the ordering test pass on
  // the stub's FIFO timers rather than on the code under test.
  let opens = 0;
  const openDelay = () => Math.max(0, 30 - opens++ * 10);
  const store = {
    get: (key: string) => request(() => rows.get(key)),
    put: (value: { key: string }) => request(() => void rows.set(value.key, value)),
    delete: (key: string) => request(() => void rows.delete(key)),
  };
  const db = {
    objectStoreNames: { contains: () => true },
    createObjectStore: () => store,
    transaction: () => ({ objectStore: () => store }),
    close: () => {},
  };
  (globalThis as Record<string, unknown>).indexedDB = {
    open: () => request(() => db, openDelay()),
  };
  return () => delete (globalThis as Record<string, unknown>).indexedDB;
}

afterEach(() => {
  delete (globalThis as Record<string, unknown>).indexedDB;
});

describe("captureFiles", () => {
  it("reports no cache rather than failing when the browser has no IndexedDB", async () => {
    expect(typeof indexedDB).toBe("undefined");
    await expect(loadPendingFiles()).resolves.toEqual([]);
    // The writes have to be no-ops too: a private window must not break the draft mirror.
    await expect(savePendingFiles([file("shot.png")])).resolves.toBeUndefined();
    await expect(clearPendingFiles()).resolves.toBeUndefined();
  });

  it("brings pasted screenshots back after a reload", async () => {
    stubIndexedDb();
    await savePendingFiles([file("shot.png"), file("trace.log")]);
    const restored = await loadPendingFiles();
    expect(restored.map((f) => f.name)).toEqual(["shot.png", "trace.log"]);
    expect(restored[0].dataUrl).toBe("data:image/png;base64,shot.png");
  });

  it("drops the cache on an empty list, so removing the last file sticks", async () => {
    stubIndexedDb();
    await savePendingFiles([file("shot.png")]);
    await savePendingFiles([]);
    await expect(loadPendingFiles()).resolves.toEqual([]);
  });

  /**
   * A confirmed create clears the cache and then remounts the dialog, which immediately reads it
   * back. Both calls `await` an `open` first, so without the queue the read can reach the store
   * before the delete and hand the blank form the files that were just created.
   */
  it("runs a clear before a read that was issued after it", async () => {
    stubIndexedDb();
    await savePendingFiles([file("shot.png")]);
    const cleared = clearPendingFiles();
    const reloaded = loadPendingFiles();
    await cleared;
    await expect(reloaded).resolves.toEqual([]);
  });
});
