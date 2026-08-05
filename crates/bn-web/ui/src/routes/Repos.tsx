import { Link } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ChevronRight, Flame } from "lucide-react";
import { queryKeys, reposQuery } from "../api/client";
import { warmRepoPool } from "../api/writes";
import type { RepoPool, RepoRollup } from "../api/types";
import { useNow } from "../hooks/useNow";
import { timeAgo, fmtAic } from "../lib/format";
import { pushToast } from "../lib/toast";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { Skeleton } from "../components/ui/skeleton";
import { cn } from "../lib/utils";

/** A small metadata tag on a card's meta row. */
function Tag({
  children,
  className,
  title,
}: {
  children: React.ReactNode;
  className?: string;
  title?: string;
}) {
  return (
    <span
      title={title}
      className={cn(
        "inline-flex items-center rounded-md bg-muted px-1.5 py-0.5 text-xs text-muted-foreground",
        className,
      )}
    >
      {children}
    </span>
  );
}

/** Warm-pool trigger (#497): fire `bn wt pool warm --repo <repo>` — long-running, so the server
 * kicks it off detached and acks "started"; we toast the outcome and invalidate the repos query so
 * the refreshed slot counts appear. Lives inside the card `<Link>`, so the click is stopped from
 * bubbling into a navigation. */
function WarmButton({ repo }: { repo: string }) {
  const qc = useQueryClient();
  const mut = useMutation({
    mutationFn: () => warmRepoPool(repo),
    onSuccess: (r) => {
      pushToast(r.message ?? `Warming ${repo}`, "success");
      void qc.invalidateQueries({ queryKey: queryKeys.repos });
    },
    onError: (e) => {
      pushToast(`warm ${repo} failed: ${(e as Error).message}`, "error");
    },
  });
  return (
    <Button
      type="button"
      variant="secondary"
      size="sm"
      disabled={mut.isPending}
      title={`Pre-build ${repo}'s pool slots (\`bn wt pool warm\`)`}
      onClick={(e) => {
        // Don't let the tap navigate into the repo (the button sits inside the card link).
        e.preventDefault();
        e.stopPropagation();
        if (window.confirm(`Warm ${repo}'s pool now? Builds every slot (long-running).`)) mut.mutate();
      }}
    >
      <Flame className="h-3.5 w-3.5" />
      {mut.isPending ? "warming…" : "warm pool"}
    </Button>
  );
}

/** The warm-pool strip (#497): a compact warm / leased / total indicator + a Warm button for a
 * pooled repo; a muted "pooling off" for one with no pool configured. */
function PoolStrip({ repo, pool, now }: { repo: string; pool: RepoPool; now: number }) {
  if (!pool.enabled) {
    return (
      <div className="mt-2">
        <Badge variant="muted">pooling off</Badge>
      </div>
    );
  }
  return (
    <div className="mt-2 flex flex-wrap items-center gap-1.5">
      <Badge variant={pool.warm > 0 ? "ok" : "muted"} title="warm idle slots ready to lease">
        <Flame className="h-3 w-3" />
        {pool.warm}/{pool.total} warm
      </Badge>
      {pool.leased > 0 && <Tag>{pool.leased} leased</Tag>}
      {pool.last_warmed_ts != null && (
        <Tag title="last warmed">warmed {timeAgo(pool.last_warmed_ts, now)}</Tag>
      )}
      <span className="flex-1" />
      <WarmButton repo={repo} />
    </div>
  );
}

/** One repo card: name · a live-active chip · a rollup meta strip (sessions · open PRs · managed ·
 * 30d AIC · last activity) · a warm-pool strip (#497). Taps through to the repo's session list.
 * Reuses the Fleet card surface so the Repos board scans like the rest of the dashboard. */
function RepoCard({ repo, now }: { repo: RepoRollup; now: number }) {
  const plural = (n: number, w: string) => `${n} ${w}${n === 1 ? "" : "s"}`;
  return (
    <Card className="p-3 transition-colors hover:border-border/80 hover:bg-accent/40">
      <Link to={`/repos/${encodeURIComponent(repo.repo)}`} className="block">
        <div className="flex items-center gap-2">
          <span className="truncate font-mono text-sm font-semibold text-foreground">{repo.repo}</span>
          {repo.active_agents > 0 && (
            <Badge variant="ok" className="shrink-0">
              {repo.active_agents} active
            </Badge>
          )}
          <span className="flex-1" />
          <ChevronRight className="h-4 w-4 shrink-0 text-muted-foreground" />
        </div>
        <div className="mt-2 flex flex-wrap items-center gap-1.5">
          <Tag>{plural(repo.session_count, "session")}</Tag>
          {repo.open_prs > 0 && <Tag>{plural(repo.open_prs, "PR")}</Tag>}
          {repo.managed && <Tag>managed</Tag>}
          {repo.aic != null && <Tag>{fmtAic(repo.aic)} AIC</Tag>}
          {repo.last_activity_ts > 0 && (
            <Tag className="text-muted-foreground/70">{timeAgo(repo.last_activity_ts, now)}</Tag>
          )}
        </div>
      </Link>
      <PoolStrip repo={repo.repo} pool={repo.pool} now={now} />
    </Card>
  );
}

/**
 * `/repos` — the multi-repo navigation layer (#495, Epic #488). Every repo bn knows (managed-repos
 * config + branch-note store + live fleet + orchestrate runs), each a card with a per-repo rollup:
 * active agents, sessions, open PRs, cost, and last activity. Drilling into a card opens that repo's
 * session list (`/repos/:repo`). Hydrated from `/api/repos`, patched by the fallback poll.
 */
export function Repos() {
  const repos = useQuery(reposQuery());
  const now = useNow();
  const list = repos.data?.repos ?? [];

  return (
    <div className="mx-auto max-w-3xl space-y-5 pb-8">
      <header className="space-y-1">
        <h2 className="text-2xl font-semibold tracking-tight">Repos</h2>
        <p className="text-sm text-muted-foreground">
          {repos.isLoading ? "loading…" : `${list.length} repo${list.length === 1 ? "" : "s"} · live`}
        </p>
      </header>

      {repos.isLoading && (
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2" aria-hidden="true">
          {[0, 1, 2, 3].map((i) => (
            <Card key={i} className="space-y-2 p-3">
              <Skeleton className="h-4 w-1/2" />
              <Skeleton className="h-3 w-2/3" />
              <Skeleton className="h-6 w-1/3" />
            </Card>
          ))}
        </div>
      )}

      {repos.isError && (
        <Card className="border-urgent/30 bg-urgent-muted p-4 text-sm text-urgent">
          Could not load repos.
        </Card>
      )}

      {!repos.isLoading && list.length === 0 && (
        <Card className="flex flex-col items-center gap-2 px-6 py-12 text-center">
          <h3 className="text-lg font-semibold">No repos yet</h3>
          <p className="max-w-sm text-sm text-muted-foreground">
            bn isn&apos;t managing any repos and has no branch notes yet.
          </p>
        </Card>
      )}

      {list.length > 0 && (
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          {list.map((r) => (
            <RepoCard key={r.repo} repo={r} now={now} />
          ))}
        </div>
      )}
    </div>
  );
}
