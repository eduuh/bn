import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiError, warmRepoPool } from "./writes";

/** A JSON `Response` stand-in for the mocked `fetch`. */
const res = (ok: boolean, status: number, body: unknown) =>
  ({ ok, status, json: async () => body }) as unknown as Response;

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("warmRepoPool (#497)", () => {
  it("POSTs to the repo's pool-warm endpoint and unwraps the started ack", async () => {
    const fetchMock = vi.fn(async () =>
      res(true, 200, {
        ok: true,
        command: "repo_pool_warm",
        data: { repo: "bn", status: "started", pid: 4242, message: "warming pool for bn (running in the background)" },
        error: null,
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    const out = await warmRepoPool("bn");
    // Correct method + URL (repo path-encoded).
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/repos/bn/pool/warm",
      expect.objectContaining({ method: "POST" }),
    );
    expect(out).toEqual({
      repo: "bn",
      status: "started",
      pid: 4242,
      message: "warming pool for bn (running in the background)",
    });
  });

  it("throws an ApiError carrying the envelope code when the repo isn't pooled", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        res(false, 400, {
          ok: false,
          command: "repo_pool_warm",
          data: null,
          error: { code: "not_pooled", message: "repo is not pooled — add it to the pools config first" },
        }),
      ),
    );
    await expect(warmRepoPool("forge")).rejects.toMatchObject({
      name: "ApiError",
      status: 400,
      code: "not_pooled",
    });
    await expect(warmRepoPool("forge")).rejects.toBeInstanceOf(ApiError);
  });
});
