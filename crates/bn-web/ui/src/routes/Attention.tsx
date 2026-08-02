import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowUpRight, Check, CheckCircle2, ClipboardList, Clock, Trash2 } from "lucide-react";
import { attnQuery, queryKeys } from "../api/client";
import type { AttnItem } from "../api/types";
import { attnAction, type AttnAction } from "../api/writes";
import { pushToast } from "../lib/toast";
import { ToastStack } from "../components/ToastStack";
import { Badge, type BadgeProps } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { Skeleton } from "../components/ui/skeleton";
import { cn } from "../lib/utils";
import { PageHead } from "../components/PageHead";

/** Map a severity to a chip variant + the card's left accent bar colour. */
const SEVERITY: Record<string, { variant: BadgeProps["variant"]; bar: string }> = {
  urgent: { variant: "urgent", bar: "bg-urgent" },
  warn: { variant: "warn", bar: "bg-warn" },
  info: { variant: "info", bar: "bg-info" },
};
const severityOf = (s: string) => SEVERITY[s] ?? { variant: "muted" as const, bar: "bg-border" };

/** One attention item with its inline resolve controls. Ack (done), Plan (→ branch todo), Snooze
 * (until tomorrow), Remove (dismiss). Each hits `POST /api/attention/:id` and invalidates the feeds
 * the badge + reviews read from, so a resolved item leaves the list at once. */
function AttnCard({ item }: { item: AttnItem }) {
  const qc = useQueryClient();
  const { variant, bar } = severityOf(item.severity);

  const invalidate = () => {
    void qc.invalidateQueries({ queryKey: queryKeys.attn });
    void qc.invalidateQueries({ queryKey: queryKeys.reviews });
    void qc.invalidateQueries({ queryKey: queryKeys.fleet });
  };
  const onError = (e: unknown, label: string) => {
    pushToast(`${label} failed: ${(e as Error).message}`, "error");
  };
  const act = (action: AttnAction, opts?: { until?: string }) => attnAction(item.id, action, opts);

  const mut = useMutation({
    mutationFn: ({ action, opts }: { action: AttnAction; opts?: { until?: string }; verb: string }) =>
      act(action, opts),
    onSuccess: (_d, v) => {
      pushToast(`${v.verb} ${item.id}`, "success");
      invalidate();
    },
    onError: (e, v) => onError(e, v.verb),
  });
  const busy = mut.isPending;

  /** Tomorrow, as `YYYY-MM-DD` (local) — the snooze target. */
  const tomorrow = () => {
    const d = new Date();
    d.setDate(d.getDate() + 1);
    return d.toISOString().slice(0, 10);
  };

  const where = item.repo ? `${item.repo}${item.branch ? `/${item.branch}` : ""}` : null;

  return (
    <Card
      className="relative overflow-hidden p-3 pl-4"
      role="article"
      aria-label={`${item.severity} attention: ${item.title}`}
    >
      <span className={cn("absolute inset-y-0 left-0 w-1", bar)} aria-hidden="true" />
      <div className="flex flex-wrap items-center gap-2">
        <Badge variant={variant} className="uppercase tracking-wide">
          {item.severity}
        </Badge>
        <span className="min-w-0 flex-1 font-medium leading-tight text-foreground">{item.title}</span>
        <span className="shrink-0 font-mono text-xs text-muted-foreground/80">{item.id}</span>
      </div>

      <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
        <span>{item.source}</span>
        {where && <span className="font-mono text-muted-foreground/90">{where}</span>}
        {item.link && (
          <a
            href={item.link}
            target="_blank"
            rel="noreferrer"
            className="inline-flex items-center gap-0.5 text-primary"
          >
            open <ArrowUpRight className="h-3 w-3" />
          </a>
        )}
      </div>

      {item.detail && (
        <pre className="mt-2 max-h-64 overflow-auto rounded-md border border-border bg-muted/60 p-3 font-mono text-xs text-muted-foreground">
          {item.detail}
        </pre>
      )}

      <div className="mt-3 flex flex-wrap gap-2">
        <Button
          size="sm"
          disabled={busy}
          onClick={() => mut.mutate({ action: "ack", verb: "Acked" })}
        >
          <Check className="h-4 w-4" /> Ack
        </Button>
        <Button
          variant="secondary"
          size="sm"
          disabled={busy}
          title="promote to a branch-note todo"
          onClick={() => mut.mutate({ action: "plan", verb: "Planned" })}
        >
          <ClipboardList className="h-4 w-4" /> Plan
        </Button>
        <Button
          variant="secondary"
          size="sm"
          disabled={busy}
          title="hide until tomorrow"
          onClick={() => mut.mutate({ action: "snooze", opts: { until: tomorrow() }, verb: "Snoozed" })}
        >
          <Clock className="h-4 w-4" /> Snooze
        </Button>
        <Button
          variant="destructive"
          size="sm"
          disabled={busy}
          title="dismiss as raised in error"
          onClick={() => {
            if (window.confirm(`Remove attention item ${item.id}?`))
              mut.mutate({ action: "remove", verb: "Removed" });
          }}
        >
          <Trash2 className="h-4 w-4" /> Remove
        </Button>
      </div>
    </Card>
  );
}

/** The loading skeleton — placeholder cards so the page never flashes an empty body (#544). */
function LoadingSkeleton() {
  return (
    <div className="grid grid-cols-1 gap-3 sm:grid-cols-2" aria-hidden="true">
      {[0, 1, 2].map((i) => (
        <Card key={i} className="space-y-2 p-3 pl-4">
          <Skeleton className="h-4 w-2/3" />
          <Skeleton className="h-3 w-1/3" />
          <Skeleton className="h-8 w-full" />
        </Card>
      ))}
    </div>
  );
}

/**
 * `/attention` — the attention list (#483): everything raised by hand, a cron script, or an agent
 * that needs you (`bn attn` / `bn doctor`). Each item resolves inline — Ack it, Plan it onto a
 * branch note, Snooze it, or Remove it — with a paired token. Urgent leads.
 */
export function Attention() {
  const attn = useQuery(attnQuery());

  const items = [...(attn.data?.items ?? [])].sort((a, b) => {
    const rank = (s: string) => (s === "urgent" ? 0 : s === "warn" ? 1 : 2);
    return rank(a.severity) - rank(b.severity) || b.created - a.created;
  });

  return (
    <div className="space-y-5 pb-8">
      <PageHead title="Attention" sub={attn.isLoading ? "loading…" : `${items.length} open`} />

      {attn.isLoading && <LoadingSkeleton />}

      {attn.isError && (
        <Card className="flex items-center gap-2 border-urgent/30 bg-urgent-muted p-4 text-sm text-urgent">
          <ArrowUpRight className="h-4 w-4" />
          Could not load the attention list.
        </Card>
      )}

      {!attn.isLoading && !attn.isError && items.length === 0 && (
        <Card className="flex flex-col items-center gap-2 px-6 py-12 text-center">
          <CheckCircle2 className="h-8 w-8 text-muted-foreground" />
          <h3 className="text-lg font-semibold">Nothing needs you</h3>
          <p className="max-w-sm text-sm text-muted-foreground">
            The attention list is clear. Items raised by hand, cron, or an agent show up here.
          </p>
        </Card>
      )}

      {items.length > 0 && (
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          {items.map((it) => (
            <AttnCard key={it.id} item={it} />
          ))}
        </div>
      )}

      <ToastStack />
    </div>
  );
}
