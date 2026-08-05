import { describe, expect, it } from "vitest";
import { pendingReviewFor } from "./review";
import type { Review } from "../api/types";

function review(over: Partial<Review>): Review {
  return {
    id: "r1",
    repo: "bn",
    branch: "dispatch/web-ui",
    plan: "the plan",
    status: "pending",
    created: 0,
    updated: 0,
    ...over,
  };
}

describe("pendingReviewFor", () => {
  it("matches a pending review for the agent by repo + note-key branch", () => {
    const reviews = [
      review({ id: "r1", repo: "bn", branch: "dispatch/web-ui" }),
      review({ id: "r2", repo: "other", branch: "dispatch/web-ui" }),
    ];
    // The live fleet reports the dashed branch; the raw `dispatch/web-ui` still matches.
    expect(pendingReviewFor(reviews, "bn", "dispatch/web-ui")?.id).toBe("r1");
    expect(pendingReviewFor(reviews, "BN", "dispatch-web-ui")?.id).toBe("r1");
  });

  it("ignores reviews that are no longer pending", () => {
    const reviews = [review({ id: "r1", status: "approved", branch: "feat-x" })];
    expect(pendingReviewFor(reviews, "bn", "feat-x")).toBeUndefined();
    const changes = [review({ id: "r2", status: "changes-requested", branch: "feat-x" })];
    expect(pendingReviewFor(changes, "bn", "feat-x")).toBeUndefined();
  });

  it("returns undefined when no review is for this agent", () => {
    const reviews = [review({ repo: "bn", branch: "other" })];
    expect(pendingReviewFor(reviews, "bn", "feat-x")).toBeUndefined();
  });
});

