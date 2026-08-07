import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { AlertTriangle, Check, Inbox, Pencil, X } from "lucide-react";
import { reviewsQuery } from "../api/client";
import type { Review, ReviewStatus } from "../api/types";
import { decideReview } from "../api/writes";
import { useAppMutation } from "../hooks/useAppMutation";
import { affects } from "../lib/invalidation";
import { setReviewStatus } from "../lib/optimistic";
import { Badge, type BadgeProps } from "./ui/badge";
import { Button } from "./ui/button";
import { Card } from "./ui/card";
import { Skeleton } from "./ui/skeleton";
import { Textarea } from "./ui/textarea";
import { cn } from "../lib/utils";

const STATUS_TONE: Record<ReviewStatus, { variant: BadgeProps["variant"]; bar: string }> = {
  pending: { variant: "warn", bar: "bg-warn" },
  approved: { variant: "ok", bar: "bg-ok" },
  "changes-requested": { variant: "warn", bar: "bg-warn" },
  rejected: { variant: "urgent", bar: "bg-urgent" },
};

/** The first non-empty line of a plan — a one-line preview for decided reviews. */
function planPreview(plan: string): string {
  return (
    plan
      .split("\n")
      .map((l) => l.trim())
      .find((l) => l.length > 0) ?? ""
  );
}

/** One review in the inbox: the plan (full for pending, a preview once decided) plus the inline
 * Approve / Request-changes / Reject controls. Each decision hits `POST /api/reviews/:id` (the
 * audited native store write) and invalidates the feeds so the agent's unblocked state shows. */
function ReviewCard({ review }: { review: Review }) {
  const [notes, setNotes] = useState("");
  const pending = review.status === "pending";
  const tone = STATUS_TONE[review.status] ?? { variant: "muted" as const, bar: "bg-border" };

  // The badge and the controls both key off `status`, so patching it flips the whole card at once —
  // the decision reads as taken the moment you click it.
  const approveMut = useAppMutation({
    mutationFn: () => decideReview(review.id, "approve"),
    optimistic: setReviewStatus(review.id, "approved"),
    invalidates: affects.review,
    toast: {
      success: "Plan approved — the agent may implement + open the PR",
      error: "approve failed",
    },
  });
  const changesMut = useAppMutation({
    mutationFn: () => decideReview(review.id, "request_changes", notes.trim()),
    optimistic: setReviewStatus(review.id, "changes-requested"),
    invalidates: affects.review,
    toast: {
      success: "Changes requested — the agent will revise + re-submit",
      error: "request changes failed",
    },
    onSuccess: () => setNotes(""),
  });
  const rejectMut = useAppMutation({
    mutationFn: () => decideReview(review.id, "reject"),
    optimistic: setReviewStatus(review.id, "rejected"),
    invalidates: affects.review,
    toast: { success: "Plan rejected — the agent stops", error: "reject failed" },
  });

  const busy = approveMut.isPending || changesMut.isPending || rejectMut.isPending;

  return (
    <Card
      className={cn("relative overflow-hidden p-3 pl-4", pending && "border-warn/40")}
      role="article"
      aria-label={`review ${review.id} ${review.branch}`}
    >
      <span className={cn("absolute inset-y-0 left-0 w-1", tone.bar)} aria-hidden="true" />
      <div className="flex flex-wrap items-center gap-2">
        <span className="truncate font-mono text-sm font-semibold text-foreground">
          {review.repo}
        </span>
        <Badge variant={tone.variant}>{review.status}</Badge>
        <span className="flex-1" />
        <span className="shrink-0 font-mono text-xs text-muted-foreground/80">{review.id}</span>
      </div>
      <div className="mt-1 truncate font-mono text-xs text-muted-foreground">{review.branch}</div>

      {pending ? (
        <pre className="mt-2 max-h-80 overflow-auto rounded-md border border-border bg-muted/60 p-3 font-mono text-xs leading-relaxed text-foreground/90">
          {review.plan}
        </pre>
      ) : (
        <p className="mt-2 truncate text-sm text-foreground/90">{planPreview(review.plan)}</p>
      )}

      {review.notes && (
        <div className="mt-2 rounded-md border border-border bg-muted/40 p-2 text-xs text-muted-foreground">
          <span className="font-medium text-foreground/80">notes:</span> {review.notes}
        </div>
      )}

      {pending && (
        <>
          <Textarea
            className="mt-3 min-h-[3.5rem] font-mono text-[13px]"
            rows={2}
            value={notes}
            onChange={(e) => setNotes(e.target.value)}
            placeholder="Change notes (required to request changes)…"
          />
          <div className="mt-2 flex flex-wrap gap-2">
            <Button size="sm" disabled={busy} onClick={() => approveMut.mutate()}>
              <Check className="h-4 w-4" /> Approve
            </Button>
            <Button
              variant="secondary"
              size="sm"
              disabled={busy || !notes.trim()}
              onClick={() => changesMut.mutate()}
            >
              <Pencil className="h-4 w-4" /> Request changes
            </Button>
            <Button
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
        </>
      )}
    </Card>
  );
}

/** A section header — title + a live count badge. */
function SectionHead({
  title,
  count,
  tone,
}: {
  title: string;
  count: number;
  tone?: BadgeProps["variant"];
}) {
  return (
    <div className="flex items-center gap-2">
      <h3 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">
        {title}
      </h3>
      <Badge variant={tone ?? "muted"}>{count}</Badge>
    </div>
  );
}

/** The loading skeleton — placeholder cards so the section never flashes an empty body (#544). */
function LoadingSkeleton() {
  return (
    <div className="grid grid-cols-1 gap-3 sm:grid-cols-2" aria-hidden="true">
      {[0, 1].map((i) => (
        <Card key={i} className="space-y-2 p-3 pl-4">
          <Skeleton className="h-4 w-1/2" />
          <Skeleton className="h-3 w-1/3" />
          <Skeleton className="h-20 w-full" />
        </Card>
      ))}
    </div>
  );
}

/**
 * The plan-review inbox (design §479 / #349), now a section of the Command Center (#558). Agents
 * pause here before opening a PR; the human approves, requests changes (with notes), or rejects —
 * one-tap, on a phone. Pending reviews lead; decided ones follow as history. A paired token
 * authorizes the decisions. Renders its own loading / error / empty states so it drops cleanly
 * into a tab without a wrapping page header.
 */
export function ReviewsInbox() {
  const reviews = useQuery(reviewsQuery());

  const all = reviews.data?.reviews ?? [];
  const pending = all.filter((r) => r.status === "pending");
  const decided = all.filter((r) => r.status !== "pending");

  return (
    <div className="space-y-5">
      {reviews.isLoading && <LoadingSkeleton />}

      {reviews.isError && (
        <Card className="flex items-center gap-2 border-urgent/30 bg-urgent-muted p-4 text-sm text-urgent">
          <AlertTriangle className="h-4 w-4" />
          Could not load plan reviews.
        </Card>
      )}

      {!reviews.isLoading && !reviews.isError && all.length === 0 && (
        <Card className="flex flex-col items-center gap-2 px-6 py-12 text-center">
          <Inbox className="h-8 w-8 text-muted-foreground" />
          <h3 className="text-lg font-semibold">No plan reviews</h3>
          <p className="max-w-sm text-sm text-muted-foreground">
            Agents pause here for approval before opening a PR. Nothing is waiting right now.
          </p>
        </Card>
      )}

      {pending.length > 0 && (
        <section className="space-y-3" aria-label="Pending reviews">
          <SectionHead title="Pending" count={pending.length} tone="warn" />
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            {pending.map((r) => (
              <ReviewCard key={r.id} review={r} />
            ))}
          </div>
        </section>
      )}

      {decided.length > 0 && (
        <section className="space-y-3" aria-label="Review history">
          <SectionHead title="History" count={decided.length} />
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            {decided.slice(0, 20).map((r) => (
              <ReviewCard key={r.id} review={r} />
            ))}
          </div>
        </section>
      )}
    </div>
  );
}
