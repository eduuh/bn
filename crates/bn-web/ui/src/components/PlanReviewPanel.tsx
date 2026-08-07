import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Check, MessageSquare, X } from "lucide-react";
import { reviewsQuery } from "../api/client";
import { pendingReviewFor } from "../lib/review";
import { decideReview } from "../api/writes";
import { useAppMutation } from "../hooks/useAppMutation";
import { affects } from "../lib/invalidation";
import { setReviewStatus } from "../lib/optimistic";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Card } from "./ui/card";
import { Textarea } from "./ui/textarea";

/**
 * The plan-review panel (design §4(c) / #349). Shown only when a *pending* plan-review exists for
 * this agent: it renders the real plan text with Approve / Request changes (notes) / Reject, each
 * hitting `POST /api/reviews/:id` (the audited native store write). Approve unblocks the agent's
 * poll loop (it implements + opens the PR), request-changes sends notes back for a revise, reject
 * stops it. Renders nothing when there's no pending review. The agent's own `bn review status`
 * poll picks up the decision — the panel just flips the store.
 */
export function PlanReviewPanel({ repo, branch }: { repo: string; branch: string }) {
  const reviews = useQuery(reviewsQuery());
  const [notes, setNotes] = useState("");
  const review = reviews.data ? pendingReviewFor(reviews.data.reviews, repo, branch) : undefined;

  // The three mutations below are declared before the `!review` early-return, because hooks
  // cannot be conditional — but they can only ever *fire* from buttons that render after it.
  // Re-checking here turns that invariant into a rejected promise (surfaced by the error toast)
  // instead of a non-null assertion that would throw a bare TypeError if it were ever wrong.
  const decide = (decision: "approve" | "request_changes" | "reject", notes?: string) => {
    if (!review) return Promise.reject(new Error("no pending review"));
    return decideReview(review.id, decision, notes);
  };
  // A decided review is no longer pending, so this panel closes itself on the click rather than on
  // the refetch. `review?.id ?? ""` never matches when there is none — which is also the only case
  // where `decide` rejects, so the patch and the request agree about doing nothing.
  const id = review?.id ?? "";

  const approveMut = useAppMutation({
    mutationFn: () => decide("approve"),
    optimistic: setReviewStatus(id, "approved"),
    invalidates: affects.review,
    toast: {
      success: "plan approved — the agent may implement + open the PR",
      error: "approve failed",
    },
  });
  const changesMut = useAppMutation({
    mutationFn: () => decide("request_changes", notes.trim()),
    optimistic: setReviewStatus(id, "changes-requested"),
    invalidates: affects.review,
    toast: {
      success: "changes requested — the agent will revise + re-submit",
      error: "request changes failed",
    },
    onSuccess: () => setNotes(""),
  });
  const rejectMut = useAppMutation({
    mutationFn: () => decide("reject"),
    optimistic: setReviewStatus(id, "rejected"),
    invalidates: affects.review,
    toast: { success: "plan rejected — the agent stops", error: "reject failed" },
  });

  if (!review) return null;
  const busy = approveMut.isPending || changesMut.isPending || rejectMut.isPending;

  return (
    <Card className="space-y-3 border-warn/40 p-4">
      <div className="flex flex-wrap items-center gap-2">
        <h3 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">
          Plan review
        </h3>
        <Badge variant="warn">waiting</Badge>
        <span className="font-mono text-xs text-muted-foreground">{review.id}</span>
      </div>
      <p className="text-xs text-muted-foreground">
        This agent proposed a plan and is paused before implementing / opening a PR. Approve to
        unblock it, request changes to send notes back, or reject to stop it. Actions are audited.
      </p>
      <pre className="max-h-72 overflow-auto rounded-md border border-border bg-muted/60 p-3 font-mono text-xs text-foreground/90">
        {review.plan}
      </pre>
      <Textarea
        className="min-h-[4.5rem] font-mono text-[13px] leading-relaxed"
        value={notes}
        onChange={(e) => setNotes(e.target.value)}
        placeholder="Change notes (sent to the agent on Request changes)…"
        rows={3}
      />
      <div className="flex flex-wrap gap-2">
        <Button type="button" size="sm" disabled={busy} onClick={() => approveMut.mutate()}>
          <Check className="h-4 w-4" /> Approve
        </Button>
        <Button
          type="button"
          variant="secondary"
          size="sm"
          disabled={busy || !notes.trim()}
          onClick={() => changesMut.mutate()}
        >
          <MessageSquare className="h-4 w-4" /> Request changes
        </Button>
        <Button
          type="button"
          variant="destructive"
          size="sm"
          disabled={busy}
          onClick={() => {
            if (window.confirm("Reject the plan and stop the agent?")) rejectMut.mutate();
          }}
        >
          <X className="h-4 w-4" /> Reject
        </Button>
      </div>
    </Card>
  );
}
