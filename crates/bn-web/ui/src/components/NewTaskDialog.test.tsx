import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { NewTaskDialog } from "./NewTaskDialog";

/**
 * The capture dialog's file handling, which is where a screenshot can quietly go missing or
 * quietly double.
 *
 * The editor itself is behind `lazy()` and never resolves under jsdom, so what is rendered here is
 * the dialog around its Suspense fallback. That is the right surface anyway: both behaviours under
 * test are the *dialog's* paste/drop handlers, which exist precisely because they fire from
 * anywhere in the form rather than only inside the editor.
 */

vi.mock("../lib/captureFiles", () => ({
  loadPendingFiles: () => Promise.resolve([]),
  savePendingFiles: () => Promise.resolve(),
  clearPendingFiles: () => Promise.resolve(),
}));

afterEach(() => {
  cleanup();
  // jsdom serves an opaque origin here, so `localStorage` is not always there — the same reason
  // `capture.ts` wraps every access. A missing store is a clean slate, which is what this wants.
  try {
    window.localStorage?.clear();
  } catch {
    /* nothing persisted, nothing to clear */
  }
});

function open() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <NewTaskDialog open onOpenChange={() => {}} onSubmit={() => {}} />
    </QueryClientProvider>,
  );
  const dialog = screen.getByRole("dialog");
  return dialog;
}

/** A paste carrying one file, dispatched the way a browser dispatches it: bubbling and cancelable. */
function pasteFile(target: Element, file: File, { handledFirst = false } = {}) {
  const event = new Event("paste", { bubbles: true, cancelable: true });
  Object.defineProperty(event, "clipboardData", {
    value: { files: [file], items: [], getData: () => "", types: ["Files"] },
  });
  // BlockNote's ProseMirror plugin calls `preventDefault()` on every paste inside the editor and
  // routes the file through `uploadFile`. It does not stop propagation, so the event still reaches
  // the dialog — reproducing that here is the whole point of the test.
  if (handledFirst) target.addEventListener("paste", (e) => e.preventDefault(), { once: true });
  target.dispatchEvent(event);
}

const png = () => new File([new Uint8Array([1, 2, 3])], "shot.png", { type: "image/png" });

/** Let the `FileReader` a paste kicks off finish, and React re-render on the back of it. */
const settle = () => act(() => new Promise<void>((resolve) => setTimeout(resolve, 50)));

describe("NewTaskDialog file capture", () => {
  it("records a screenshot pasted anywhere in the form", async () => {
    const dialog = open();
    pasteFile(dialog, png());
    expect(await screen.findByText("shot.png")).toBeTruthy();
  });

  it("does not re-record a paste the editor has already handled", async () => {
    const dialog = open();
    pasteFile(dialog, png(), { handledFirst: true });
    // A negative has nothing to wait *for*, and `waitFor` would pass on its first synchronous
    // check — before the `FileReader` an un-guarded handler starts has resolved, which is exactly
    // when the assertion is still true either way. Settle the reader first, then look.
    await settle();
    expect(screen.queryByText("Attachments")).toBeNull();
    expect(screen.queryByText("shot.png")).toBeNull();
  });

  it("de-duplicates the names of two screenshots pasted in a row", async () => {
    const dialog = open();
    pasteFile(dialog, png());
    expect(await screen.findByText("shot.png")).toBeTruthy();
    pasteFile(dialog, png());
    expect(await screen.findByText("shot-2.png")).toBeTruthy();
  });
});
