import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { ChevronRight, ExternalLink, Flame, Minus, Plus, Wrench } from "lucide-react";
import { reposQuery } from "../api/client";
import {
  setRepoPool,
  startRepoSetupAgent,
  warmRepoPool,
  type RepoSetupAgentResult,
} from "../api/writes";
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

/** What the pool payload still carries. The prose rules and the board-level policy card they fed
 * are gone; `min_ready` stays because the optimistic pool update recomputes `needs_top_up` from it
 * (see `lib/optimistic`). */
type RepoPoolPolicy = {
  min_ready: number;
};

type PolicyRepoPool = RepoPool & {
  ready: number;
  worker_idle: number;
  needs_top_up: boolean;
  full: boolean;
  policy: RepoPoolPolicy;
};

type PolicyRepoRollup = RepoRollup & { pool: PolicyRepoPool };
type PolicyReposView = { repos: PolicyRepoRollup[] };

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

function SetupButton({ repo }: { repo: string }) {
  const mut = useAppMutation({
    mutationFn: () => startRepoSetupAgent(repo),
    invalidates: affects.agentLaunch,
    toast: {
      success: (r) => setupOutcome(r, repo),
      error: `Could not start ${repo} setup`,
    },
  });
  return (
    <Button
      type="button"
      variant="secondary"
      size="sm"
      disabled={mut.isPending}
      title={`Start ${repo}'s setup agent`}
      onClick={(e) => {
        e.preventDefault();
        e.stopPropagation();
        if (window.confirm(`Start a repo setup agent for ${repo}?`)) mut.mutate();
      }}
    >
      <Wrench className="h-3.5 w-3.5" />
      {mut.isPending ? "starting…" : "setup"}
    </Button>
  );
}

/** What to tell the user a setup request actually did. `bn dispatch` may launch the agent now or
 * file it in the queue for the tick to start; saying "starting…" for the second is the reason a
 * click could look like it had done nothing. */
function setupOutcome(result: RepoSetupAgentResult, repo: string): string {
  return result?.dispatched === false
    ? `Queued repo setup for ${repo} — it starts when the queue has capacity`
    : `Starting repo setup for ${repo}…`;
}

/** How many repos still need setup — stated, not actionable in bulk.
 *
 * There used to be a button here that started a setup agent for every one of them. One click could
 * put ten agents in the queue, which is a lot of machine to commit from a header, and the per-repo
 * button on the repo page is the same work with the blast radius a person can actually judge. */
function SetupNeededCount({ count }: { count: number }) {
  if (count === 0) return null;
  return (
    <span
      className="flex items-center gap-1.5 font-mono text-[11px] tabular-nums text-muted-foreground/70"
      title="Repos whose bn build/verify scripts are missing — open one to start its setup agent"
    >
      <Wrench className="h-3.5 w-3.5" />
      {count} need setup
    </span>
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
        title="idle warm slots ready out of configured pool size"
      >
        <Flame className="h-3 w-3" />
        {pool.ready}/{pool.total} ready
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
          {repo.setup?.needs_setup && <Tag className="text-warn">needs setup</Tag>}
          {repo.aic != null && <Tag>{fmtAic(repo.aic)} AIC</Tag>}
          {repo.last_activity_ts > 0 && (
            <Tag className="text-muted-foreground/70">{timeAgo(repo.last_activity_ts, now)}</Tag>
          )}
        </div>
      </Link>
      <ForgeLinkRow links={repo.links} />
      {repo.setup?.needs_setup && (
        <div className="mt-2 flex justify-end">
          <SetupButton repo={repo.repo} />
        </div>
      )}
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
  const setupNeeded = list.filter((repo) => repo.setup?.needs_setup).length;

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="flex flex-none items-center gap-3 border-b border-border px-4 py-2.5">
        <div className="flex min-w-0 flex-1 items-baseline gap-3">
          <h1 className="font-display text-lg font-semibold tracking-tight text-foreground">
            Repos
          </h1>
          <span className="font-mono text-[11px] tabular-nums text-muted-foreground/70">
            {repos.isLoading
              ? "loading…"
              : `${list.length} repo${list.length === 1 ? "" : "s"} · live`}
          </span>
        </div>
        <SetupNeededCount count={setupNeeded} />
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
          <div className={GRID}>
            {list.map((r) => (
              <RepoCard key={r.repo} repo={r} now={now} />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
