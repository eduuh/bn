import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { ChevronRight, ExternalLink, Flame, Minus, Plus } from "lucide-react";
import { reposQuery } from "../api/client";
import { setRepoPool, warmRepoPool } from "../api/writes";
import { MAX_POOL_SIZE, MIN_POOL_SIZE, type RepoPool, type RepoRollup } from "../api/types";
import { useAppMutation } from "../hooks/useAppMutation";
import { affects } from "../lib/invalidation";
import { setRepoPoolSize } from "../lib/optimistic";
import { useNow } from "../hooks/useNow";
import { timeAgo, fmtAic } from "../lib/format";
import { forgeLinks } from "../lib/repos";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { Skeleton } from "../components/ui/skeleton";
import { cn } from "../lib/utils";

type RepoPoolPolicy = {
  capacity: "hard";
  min_ready: number;
  controller_slot: number;
  warming: "queued-heavy";
  auto_queue: "enable-resize-lease-top-up";
  leases: "active-work";
  reservations: "not-supported";
  stale_recovery: "clean-dead-only";
  rules: string[];
};

type PolicyRepoPool = RepoPool & {
  ready: number;
  worker_idle: number;
  needs_top_up: boolean;
  full: boolean;
  policy: RepoPoolPolicy;
};

type PolicyRepoRollup = RepoRollup & { pool: PolicyRepoPool };
type PolicyReposView = { repos: PolicyRepoRollup[]; pool_policy?: RepoPoolPolicy };

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

/** Warm-pool trigger (#497): queue the repo's heavy `pool-warm` job instead of starting an uncapped
 * detached build. Lives inside the card `<Link>`, so the click is stopped from bubbling into a
 * navigation. */
function WarmButton({ repo }: { repo: string }) {
  // The slot counts move only once the queue has admitted and completed the warm, so there is
  // nothing honest to patch optimistically.
  const mut = useAppMutation({
    mutationFn: () => warmRepoPool(repo),
    invalidates: affects.pool,
    toast: { success: `Queued pool warm for ${repo}`, error: `Could not queue ${repo} pool warm` },
  });
  return (
    <Button
      type="button"
      variant="secondary"
      size="sm"
      disabled={mut.isPending}
      title={`Queue ${repo}'s pool warm job`}
      onClick={(e) => {
        // Don't let the tap navigate into the repo (the button sits inside the card link).
        e.preventDefault();
        e.stopPropagation();
        if (
          window.confirm(`Queue a pool warm for ${repo}? Builds slots when the queue has capacity.`)
        )
          mut.mutate();
      }}
    >
      <Flame className="h-3.5 w-3.5" />
      {mut.isPending ? "queueing…" : "queue warm"}
    </Button>
  );
}

/** Turn pooling on / off / resize for a repo. Enabling writes the pools config the idle `pool-warm`
 * cron reads and queues an immediate pool warm, so one click is the whole setup without building on
 * the request path. Sits inside the card `<Link>`, so clicks are stopped from bubbling into a
 * navigation, same as the warm button. */
function PoolSizeControl({ repo, pool }: { repo: string; pool: RepoPool }) {
  const mut = useAppMutation<{ message: string }, number | null>({
    mutationFn: (size) => setRepoPool(repo, size),
    optimistic: setRepoPoolSize<number | null>(repo, (size) => size),
    invalidates: affects.pool,
    toast: { success: (r) => r.message, error: repo },
  });
  const stop = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
  };
  if (!pool.enabled) {
    return (
      <Button
        type="button"
        variant="secondary"
        size="sm"
        disabled={mut.isPending}
        title={`Pool ${repo} at ${MIN_POOL_SIZE} slots and queue its first warm`}
        onClick={(e) => {
          stop(e);
          mut.mutate(MIN_POOL_SIZE);
        }}
      >
        <Flame className="h-3.5 w-3.5" />
        {mut.isPending ? "enabling…" : "enable pool"}
      </Button>
    );
  }
  const resize = (n: number) => (e: React.MouseEvent) => {
    stop(e);
    // Slot 1 is the controller's, so MIN_POOL_SIZE is the smallest pool with a worker in it.
    // Stepping below that turns pooling off outright rather than sending a size the server would
    // refuse — but never silently: shrinking can strand slots, which `pool prune` reclaims.
    if (n < MIN_POOL_SIZE) {
      if (window.confirm(`Stop pooling ${repo}? Existing slots stay on disk.`)) mut.mutate(null);
      return;
    }
    mut.mutate(n);
  };
  return (
    <span className="inline-flex items-center gap-1">
      <Button
        type="button"
        variant="ghost"
        size="sm"
        disabled={mut.isPending}
        title={pool.total > MIN_POOL_SIZE ? `Shrink ${repo}'s pool` : `Stop pooling ${repo}`}
        onClick={resize(pool.total - 1)}
      >
        <Minus className="h-3.5 w-3.5" />
      </Button>
      <Button
        type="button"
        variant="ghost"
        size="sm"
        disabled={mut.isPending || pool.total >= MAX_POOL_SIZE}
        title={`Grow ${repo}'s pool`}
        onClick={resize(pool.total + 1)}
      >
        <Plus className="h-3.5 w-3.5" />
      </Button>
    </span>
  );
}

function poolNeedsTopUp(pool: PolicyRepoPool) {
  return pool.enabled && pool.needs_top_up;
}

function poolIsFull(pool: PolicyRepoPool) {
  return pool.enabled && pool.full;
}

/** The warm-pool strip (#497): a compact warm / leased / total indicator, a resize control and a
 * Warm button for a pooled repo; for an un-pooled one, the button that pools it. "pooling off" used
 * to be a dead badge — the config it reflects was hand-edited only, so a machine that had never
 * touched `~/.config/wt/pools` showed that state on every card with no way out of it. */
function PoolStrip({ repo, pool, now }: { repo: string; pool: PolicyRepoPool; now: number }) {
  if (!pool.enabled) {
    return (
      <div className="mt-2 flex flex-wrap items-center gap-1.5">
        <Badge variant="muted">pooling off</Badge>
        <span className="flex-1" />
        <PoolSizeControl repo={repo} pool={pool} />
      </div>
    );
  }
  return (
    <div className="mt-2 flex flex-wrap items-center gap-1.5">
      <Badge
        variant={pool.ready > 0 ? "ok" : "muted"}
        title="idle warm worker slots ready to lease"
      >
        <Flame className="h-3 w-3" />
        {pool.ready}/{Math.max(pool.total - 1, 0)} ready
      </Badge>
      <Tag title="pool-warm is queued automatically on enable, resize, and lease top-up">
        auto queue
      </Tag>
      {poolNeedsTopUp(pool) && (
        <Tag className="bg-warn-muted text-warn" title="below min-ready; pool-warm should top up">
          top-up pending
        </Tag>
      )}
      {poolIsFull(pool) && <Tag title="all worker capacity is leased">full</Tag>}
      {pool.leased > 0 && <Tag>{pool.leased} leased</Tag>}
      {pool.last_warmed_ts != null && (
        <Tag title="last warmed">warmed {timeAgo(pool.last_warmed_ts, now)}</Tag>
      )}
      <span className="flex-1" />
      <PoolSizeControl repo={repo} pool={pool} />
      <WarmButton repo={repo} />
    </div>
  );
}

function PoolPolicyCard({ policy, repos }: { policy: RepoPoolPolicy; repos: PolicyRepoRollup[] }) {
  const pooled = repos.filter((r) => r.pool.enabled).length;
  const topUp = repos.filter((r) => poolNeedsTopUp(r.pool)).length;
  const full = repos.filter((r) => poolIsFull(r.pool)).length;
  return (
    <Card className="mb-3 space-y-2 p-3">
      <div className="flex flex-wrap items-center gap-1.5">
        <span className="font-display text-sm font-semibold text-foreground">Warm-pool policy</span>
        <Tag>hard capacity</Tag>
        <Tag>min ready {policy.min_ready}</Tag>
        <Tag>auto queue</Tag>
        <span className="flex-1" />
        <Tag>{pooled} pooled</Tag>
        {topUp > 0 && <Tag className="bg-warn-muted text-warn">{topUp} need top-up</Tag>}
        {full > 0 && <Tag>{full} full</Tag>}
      </div>
      <div className="grid gap-1 text-xs text-muted-foreground sm:grid-cols-2 xl:grid-cols-3">
        {policy.rules.map((rule) => (
          <div key={rule} className="rounded-md bg-muted/60 px-2 py-1">
            {rule}
          </div>
        ))}
      </div>
    </Card>
  );
}

/** The repo's forge pages — code, issues, PRs — derived server-side from the clone's `origin`.
 * Renders nothing for a repo with no clone here or an unparseable remote, and only the links that
 * forge actually has. Deliberately *outside* the card's `<Link>`: an anchor inside an anchor is
 * invalid HTML, and the tap targets have to stay distinct on a phone. */
function ForgeLinkRow({ links }: { links: RepoRollup["links"] }) {
  const items = forgeLinks(links);
  if (items.length === 0) return null;
  return (
    <div className="mt-2 flex flex-wrap items-center gap-1.5">
      {items.map((l) => (
        <a
          key={l.href}
          href={l.href}
          target="_blank"
          rel="noreferrer"
          onClick={(e) => e.stopPropagation()}
          className="inline-flex items-center gap-1 rounded-md bg-muted px-1.5 py-0.5 text-xs text-muted-foreground hover:bg-accent hover:text-foreground"
        >
          {l.label}
          <ExternalLink className="h-3 w-3" />
        </a>
      ))}
    </div>
  );
}

/** One repo card: name · a live-active chip · a rollup meta strip (sessions · open PRs · managed ·
 * 30d AIC · last activity) · its forge links · a warm-pool strip (#497). Taps through to the repo's
 * session list. Reuses the Fleet card surface so the Repos board scans like the rest of the
 * dashboard. */
function RepoCard({ repo, now }: { repo: PolicyRepoRollup; now: number }) {
  const plural = (n: number, w: string) => `${n} ${w}${n === 1 ? "" : "s"}`;
  return (
    <Card className="p-3 transition-colors hover:border-border/80 hover:bg-accent/40">
      <Link to={`/repos/${encodeURIComponent(repo.repo)}`} className="block">
        <div className="flex items-center gap-2">
          <span className="truncate font-mono text-sm font-semibold text-foreground">
            {repo.repo}
          </span>
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
      <ForgeLinkRow links={repo.links} />
      <PoolStrip repo={repo.repo} pool={repo.pool} now={now} />
    </Card>
  );
}

/**
 * `/repos` — the local repo inventory (#495, Epic #488). Cards represent repos with local evidence:
 * a bare clone, a checked-out worktree, or a live/in-flight agent. Notes, managed config, and cost
 * data decorate those cards but do not create them. A repo bn has cloned but never worked in is a
 * 0-session card at the bottom, not a missing one. Drilling into a card opens that repo's session
 * list (`/repos/:repo`). Hydrated from `/api/repos`, patched by the fallback poll.
 */
/** The repo grid. `items-start` because grid cells otherwise stretch to their row's tallest
 * sibling, so a three-line repo card beside a nine-line one grew a void under its content. The
 * column count climbs with the viewport: at full-bleed, two columns made each card 850px wide to
 * hold four short stats. */
const GRID = "grid grid-cols-1 items-start gap-3 sm:grid-cols-2 xl:grid-cols-3 2xl:grid-cols-4";

export function Repos() {
  const repos = useQuery(reposQuery());
  const now = useNow();
  const data = repos.data as PolicyReposView | undefined;
  const list = data?.repos ?? [];
  const poolPolicy = data?.pool_policy ?? list[0]?.pool.policy;

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="flex flex-none items-baseline gap-3 border-b border-border px-4 py-2.5">
        <h1 className="font-display text-lg font-semibold tracking-tight text-foreground">Repos</h1>
        <span className="font-mono text-[11px] tabular-nums text-muted-foreground/70">
          {repos.isLoading
            ? "loading…"
            : `${list.length} repo${list.length === 1 ? "" : "s"} · live`}
        </span>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto px-4 py-4">
        {repos.isLoading && (
          <div className={GRID} aria-hidden="true">
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
          <div className="rounded-lg border border-urgent/30 bg-urgent-muted p-4 text-sm text-urgent">
            Could not load repos.
          </div>
        )}

        {!repos.isLoading && list.length === 0 && (
          <div className="flex flex-col items-center gap-2 px-6 py-12 text-center">
            <h3 className="font-display text-base font-semibold">No repos yet</h3>
            <p className="max-w-sm text-sm text-muted-foreground">
              bn isn&apos;t managing any repos and has no branch notes yet.
            </p>
          </div>
        )}

        {list.length > 0 && (
          <>
            {poolPolicy && <PoolPolicyCard policy={poolPolicy} repos={list} />}
            <div className={GRID}>
              {list.map((r) => (
                <RepoCard key={r.repo} repo={r} now={now} />
              ))}
            </div>
          </>
        )}
      </div>
    </div>
  );
}
