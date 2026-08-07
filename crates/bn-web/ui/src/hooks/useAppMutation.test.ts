// The write layer's contract. These four cases are the ones the old hand-rolled call sites got
// wrong, so they are the ones worth pinning.

import { describe, expect, it, vi, beforeEach } from "vitest";
import { QueryClient } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import React from "react";
import { QueryClientProvider } from "@tanstack/react-query";
import { useAppMutation, optimistic } from "./useAppMutation";

vi.mock("../lib/toast", () => ({ pushToast: vi.fn() }));
import { pushToast } from "../lib/toast";

interface View {
  items: string[];
}
const KEY = ["test-view"] as const;

function harness() {
  // `retry: false` so a rejected mutationFn surfaces immediately instead of being retried.
  const qc = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  const wrapper = ({ children }: { children: React.ReactNode }) =>
    React.createElement(QueryClientProvider, { client: qc }, children);
  return { qc, wrapper };
}

/** A promise whose settlement the test controls, so we can inspect the cache mid-flight. */
function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const dropFirst = optimistic<View>(KEY, (v) => ({ items: v.items.slice(1) }));

beforeEach(() => {
  vi.mocked(pushToast).mockClear();
});

describe("useAppMutation", () => {
  it("patches the cache before the server responds", async () => {
    const { qc, wrapper } = harness();
    qc.setQueryData<View>(KEY, { items: ["a", "b"] });
    const d = deferred<void>();

    const { result } = renderHook(
      () => useAppMutation({ mutationFn: () => d.promise, optimistic: dropFirst }),
      { wrapper },
    );

    result.current.mutate();

    // The point of the whole exercise: the list is already short before the write lands.
    await waitFor(() => expect(qc.getQueryData<View>(KEY)?.items).toEqual(["b"]));
    expect(result.current.isPending).toBe(true);

    d.resolve();
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
  });

  it("rolls back on error", async () => {
    const { qc, wrapper } = harness();
    qc.setQueryData<View>(KEY, { items: ["a", "b"] });

    const { result } = renderHook(
      () =>
        useAppMutation({
          mutationFn: () => Promise.reject(new Error("nope")),
          optimistic: dropFirst,
          toast: { error: "Could not drop" },
        }),
      { wrapper },
    );

    result.current.mutate();

    await waitFor(() => expect(result.current.isError).toBe(true));
    expect(qc.getQueryData<View>(KEY)?.items).toEqual(["a", "b"]);
    // A bare string error spec is a prefix on the underlying reason.
    expect(pushToast).toHaveBeenCalledWith("Could not drop: nope", "error");
  });

  it("does not roll back over a newer frame that replaced the optimistic value", async () => {
    const { qc, wrapper } = harness();
    qc.setQueryData<View>(KEY, { items: ["a", "b"] });
    const d = deferred<void>();

    const { result } = renderHook(
      () => useAppMutation({ mutationFn: () => d.promise, optimistic: dropFirst }),
      { wrapper },
    );

    result.current.mutate();
    await waitFor(() => expect(qc.getQueryData<View>(KEY)?.items).toEqual(["b"]));

    // An SSE frame lands mid-flight — this is fresher truth than our pre-mutation snapshot.
    const fromStream: View = { items: ["x", "y", "z"] };
    qc.setQueryData<View>(KEY, fromStream);

    d.reject(new Error("failed"));
    await waitFor(() => expect(result.current.isError).toBe(true));

    // Rolling back to ["a","b"] here would drag the UI backwards past the frame. Compared by value,
    // not identity: react-query's structural sharing re-builds the stored object, so the frame's
    // content is what survives, never its reference.
    expect(qc.getQueryData<View>(KEY)?.items).toEqual(["x", "y", "z"]);
  });

  it("invalidates on failure, not just on success", async () => {
    const { qc, wrapper } = harness();
    const spy = vi.spyOn(qc, "invalidateQueries");

    const { result } = renderHook(
      () =>
        useAppMutation({
          mutationFn: () => Promise.reject(new Error("boom")),
          invalidates: [["other"]],
        }),
      { wrapper },
    );

    result.current.mutate();
    await waitFor(() => expect(result.current.isError).toBe(true));

    // A failed write must still leave the screen showing server truth.
    expect(spy).toHaveBeenCalledWith({ queryKey: ["other"] });
  });

  it("can preserve local drafts by skipping declared invalidation on failure", async () => {
    const { qc, wrapper } = harness();
    const spy = vi.spyOn(qc, "invalidateQueries");

    const { result } = renderHook(
      () =>
        useAppMutation({
          mutationFn: () => Promise.reject(new Error("save failed")),
          invalidates: [["editor"]],
          invalidateOnError: false,
        }),
      { wrapper },
    );

    result.current.mutate();
    await waitFor(() => expect(result.current.isError).toBe(true));

    expect(spy).not.toHaveBeenCalled();
  });

  it("still invalidates on success when error invalidation is disabled", async () => {
    const { qc, wrapper } = harness();
    const spy = vi.spyOn(qc, "invalidateQueries");

    const { result } = renderHook(
      () =>
        useAppMutation({
          mutationFn: () => Promise.resolve(),
          invalidates: [["editor"]],
          invalidateOnError: false,
        }),
      { wrapper },
    );

    result.current.mutate();
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(spy).toHaveBeenCalledWith({ queryKey: ["editor"] });
  });

  it("normalizes non-Error rejections before reporting them", async () => {
    const { wrapper } = harness();
    const mutationFn = vi.fn<() => Promise<never>>().mockRejectedValue("plain failure");

    const { result } = renderHook(
      () =>
        useAppMutation({
          mutationFn,
          toast: { error: (e) => e.message },
        }),
      { wrapper },
    );

    result.current.mutate();
    await waitFor(() => expect(result.current.isError).toBe(true));

    expect(result.current.error).toEqual(new Error("plain failure"));
    expect(pushToast).toHaveBeenCalledWith("plain failure", "error");
  });

  it("reconciles patched keys without the call site listing them", async () => {
    const { qc, wrapper } = harness();
    qc.setQueryData<View>(KEY, { items: ["a"] });
    const spy = vi.spyOn(qc, "invalidateQueries");

    const { result } = renderHook(
      () => useAppMutation({ mutationFn: () => Promise.resolve(), optimistic: dropFirst }),
      { wrapper },
    );

    result.current.mutate();
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(spy).toHaveBeenCalledWith({ queryKey: KEY });
  });
});
