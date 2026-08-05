// Pure plan-review matching (#349): given the plan-reviews from `GET /api/reviews`, find the one a
// specific agent is currently paused on. Side-effect-free so the panel stays a thin render of this
// decision and the match rule is unit-tested (`review.test.ts`).

import type { Review } from "../api/types";
import { sanitizeBranch } from "./board";

/**
 * The *pending* plan-review for `repo:branch`, if the agent is waiting-for-review. Matches on repo
 * (case-insensitive) + note-key branch (both normalized to the dash form the live fleet uses), and
 * only returns a review still `pending` — an approved/rejected/changes-requested one no longer
 * blocks the agent, so the panel closes.
 */
export function pendingReviewFor(
  reviews: Review[],
  repo: string,
  branch: string,
): Review | undefined {
  const r = repo.toLowerCase();
  const b = sanitizeBranch(branch);
  return reviews.find(
    (rv) =>
      rv.status === "pending" && rv.repo.toLowerCase() === r && sanitizeBranch(rv.branch) === b,
  );
}
