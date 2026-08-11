import { useState } from "react";
import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import {
  ChevronRight,
  ExternalLink,
  Flame,
  Minus,
  Plus,
  Search,
  Settings2,
  Wrench,
  X,
} from "lucide-react";
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
import { timeAgo, fmtAicCompact } from "../lib/format";
import { filterRepos, forgeLinks, repoFilterCounts, type RepoFilter } from "../lib/repos";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { Input } from "../components/ui/input";
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

/** Touch-sized control. The shared `sm` button is 32px tall, which is under every thumb-target
 * guideline going; the board is a phone surface first, so its controls are 36px until there is a
 * mouse-sized viewport to shrink into. */
const TOUCH = "h-9 sm:h-8";

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
 * detached build. */
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
      className={TOUCH}
      disabled={mut.isPending}
      title={`Queue ${repo}'s pool warm job`}
      onClick={() => {
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
      className={TOUCH}
      disabled={mut.isPending}
      title={`Start ${repo}'s setup agent`}
      onClick={() => {
        if (window.confirm(`Start a repo setup agent for ${repo}?`)) mut.mutate();
      }}
    >
      <Wrench className="h-3.5 w-3.5" />
      {mut.isPending ? "starting…" : "run setup"}
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

/** Turn pooling on / off / resize for a repo. Enabling writes the pools config the idle `pool-warm`
 * cron reads and queues an immediate pool warm, so one click is the whole setup without building on
 * the request path. */
function PoolSizeControl({ repo, pool }: { repo: string; pool: RepoPool }) {
  const mut = useAppMutation<{ message: string }, number | null>({
    mutationFn: (size) => setRepoPool(repo, size),
    optimistic: setRepoPoolSize<number | null>(repo, (size) => size),
    invalidates: affects.pool,
    toast: { success: (r) => r.message, error: repo },
  });
  if (!pool.enabled) {
    return (
      <Button
        type="button"
        variant="secondary"
        size="sm"
        className={TOUCH}
        disabled={mut.isPending}
        title={`Pool ${repo} at ${MIN_POOL_SIZE} slots and queue its first warm`}
        onClick={() => mut.mutate(MIN_POOL_SIZE)}
      >
        <Flame className="h-3.5 w-3.5" />
        {mut.isPending ? "enabling…" : "enable pool"}
      </Button>
    );
  }
  const resize = (n: number) => () => {
    // Slot 1 is the controller's, so MIN_POOL_SIZE is the smallest pool with a worker in it.
    // Stepping below that turns pooling off outright rather than sending a size the server would
    // refuse — but never silently: shrinking can strand slots, which `pool prune` reclaims.
    if (n < MIN_POOL_SIZE) {
      if (window.confirm(`Stop pooling ${repo}? Existing slots stay on disk.`)) mut.mutate(null);
      return;
    }
    mut.mutate(n);
  };
  // A labelled stepper, not two bare glyph buttons: on a phone the pair used to sit at the end of a
  // wrapped row with nothing between them saying what they resized.
  return (
    <span className="inline-flex items-center gap-0.5 rounded-md border border-border bg-card p-0.5">
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className="h-8 w-8"
        disabled={mut.isPending}
        title={pool.total > MIN_POOL_SIZE ? `Shrink ${repo}'s pool` : `Stop pooling ${repo}`}
        aria-label={pool.total > MIN_POOL_SIZE ? "shrink pool" : "stop pooling"}
        onClick={resize(pool.total - 1)}
      >
        <Minus className="h-3.5 w-3.5" />
      </Button>
      <span className="min-w-[4.25rem] text-center font-mono text-xs tabular-nums text-muted-foreground">
        {pool.total} slots
      </span>
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className="h-8 w-8"
        disabled={mut.isPending || pool.total >= MAX_POOL_SIZE}
        title={`Grow ${repo}'s pool`}
        aria-label="grow pool"
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

/** The one-glance pool state, on the card face so the manage panel can stay shut. */
function PoolChip({ pool }: { pool: PolicyRepoPool }) {
  if (!pool.enabled) return <Tag title="no warm pool configured for this repo">no pool</Tag>;
  return (
    <Badge
      variant={poolNeedsTopUp(pool) ? "warn" : pool.ready > 0 ? "ok" : "muted"}
      className="shrink-0"
      title="idle warm slots ready out of configured pool size"
    >
      <Flame className="h-3 w-3" />
      {pool.ready}/{pool.total}
    </Badge>
  );
}

/** The manage panel's pool block (#497): the slot accounting, a resize stepper and a warm button
 * for a pooled repo; for an un-pooled one, the button that pools it. "pooling off" used to be a
 * dead badge — the config it reflects was hand-edited only, so a machine that had never touched
 * `~/.config/wt/pools` showed that state on every card with no way out of it. */
function PoolPanel({ repo, pool, now }: { repo: string; pool: PolicyRepoPool; now: number }) {
  return (
    <div className="space-y-2">
      {pool.enabled && (
        <div className="flex flex-wrap items-center gap-1.5">
          <Tag title="pool-warm is queued automatically on enable, resize, and lease top-up">
            auto queue
          </Tag>
          {poolNeedsTopUp(pool) && (
            <Tag
              className="bg-warn-muted text-warn"
              title="below min-ready; pool-warm should top up"
            >
              top-up pending
            </Tag>
          )}
          {poolIsFull(pool) && <Tag title="all worker capacity is leased">full</Tag>}
          {pool.leased > 0 && <Tag>{pool.leased} leased</Tag>}
          {pool.last_warmed_ts != null && (
            <Tag title="last warmed">warmed {timeAgo(pool.last_warmed_ts, now)}</Tag>
          )}
        </div>
      )}
      <div className="flex flex-wrap items-center gap-2">
        <PoolSizeControl repo={repo} pool={pool} />
        {pool.enabled && <WarmButton repo={repo} />}
      </div>
    </div>
  );
}

/** The repo's forge pages — code, issues, PRs — derived server-side from the clone's `origin`.
 * Renders nothing for a repo with no clone here or an unparseable remote, and only the links that
 * forge actually has.
 *
 * Lives in the manage panel rather than on the card face. These three chips are identical on every
 * repo that has a remote, so inline they were a second row of pure furniture per card — twelve
 * repos cost twelve rows of "GitHub · issues · pulls" before you reached the twelfth name. */
function ForgeLinkRow({ links }: { links: RepoRollup["links"] }) {
  const items = forgeLinks(links);
  if (items.length === 0) return null;
  return (
    <div className="flex min-w-0 flex-wrap items-center gap-1.5">
      {items.map((l) => (
        <a
          key={l.href}
          href={l.href}
          target="_blank"
          rel="noreferrer"
          className="inline-flex h-8 items-center gap-1 rounded-md bg-muted px-2 text-xs text-muted-foreground hover:bg-accent hover:text-foreground"
        >
          {l.label}
          <ExternalLink className="h-3 w-3" />
        </a>
      ))}
    </div>
  );
}

/** The card's second line: the rollup facts as one dim mono run, with the pool state pinned right.
 *
 * These used to be five pill tags in a `flex-wrap` row. At 360px that wrapped to three lines of
 * chips whose borders drew more ink than the numbers inside them, and the card's actual subject —
 * its name — lost the contrast fight to its own metadata. One line says the same facts in the space
 * of one line, and reads as subordinate to the name by construction.
 *
 * Everything fixed-width lives on the *other* line. A phone card has room for about thirty
 * characters here, and sharing them with a recency stamp is what truncated a six-figure spend into
 * the unreadable `147.…`; the name line has slack that this one does not, so the stamp sits up
 * there beside the name it dates. */
function MetaLine({ repo }: { repo: PolicyRepoRollup }) {
  const plural = (n: number, w: string) => `${n} ${w}${n === 1 ? "" : "s"}`;
  const parts: string[] = [plural(repo.session_count, "session")];
  if (repo.open_prs > 0) parts.push(plural(repo.open_prs, "PR"));
  if (repo.aic != null) parts.push(`${fmtAicCompact(repo.aic)} AIC`);
  if (repo.managed) parts.push("managed");
  return (
    <div className="flex items-center gap-2">
      <span
        className="truncate font-mono text-[11px] tabular-nums text-muted-foreground/80"
        title="sessions · open PRs · 30d AIC"
      >
        {parts.join(" · ")}
      </span>
      {/* Pinned right so the pool chips form a column under the recency stamps instead of hugging
          a run whose length changes on every row. */}
      <span className="ml-auto flex shrink-0 items-center">
        <PoolChip pool={repo.pool} />
      </span>
    </div>
  );
}

/** One repo card. Collapsed it is a single row: a tap target that opens the repo's session list —
 * name, live signal, recency, one meta line, its pool state — and a gear that opens everything
 * which *changes* the repo.
 *
 * The split is the point. Every card used to carry a pool stepper, a warm button, a setup button
 * and a forge row inline, which on a phone made a three-fact repo eight rows tall and put four
 * mutating controls under the thumb that was trying to scroll past it. Those buttons also sat
 * *inside* the card's `<Link>`, so each one had to cancel its own navigation on every click. Now
 * the link is a link, and its controls are siblings behind one disclosure. */
function RepoCard({ repo, now }: { repo: PolicyRepoRollup; now: number }) {
  const [manage, setManage] = useState(false);
  const needsSetup = repo.setup?.needs_setup === true;
  return (
    <Card className="overflow-hidden">
      <div className="flex items-stretch">
        <Link
          to={`/repos/${encodeURIComponent(repo.repo)}`}
          className="flex min-h-[3.5rem] min-w-0 flex-1 flex-col justify-center gap-1 px-3 py-2.5 transition-colors hover:bg-accent/40"
        >
          <div className="flex items-center gap-1.5">
            <span className="truncate font-mono text-sm font-semibold text-foreground">
              {repo.repo}
            </span>
            {repo.active_agents > 0 && (
              <Badge variant="ok" className="shrink-0">
                {repo.active_agents} active
              </Badge>
            )}
            {/* A glyph, not a `needs setup` chip: eleven of twelve repos here are missing their bn
                scripts, so the words were on nearly every row, cost a third of the meta line and
                said what the `Needs setup` filter already counts. */}
            {needsSetup && (
              <span
                title="bn build/verify scripts are missing"
                aria-label="needs setup"
                className="shrink-0 text-warn"
              >
                <Wrench className="h-3.5 w-3.5" />
              </span>
            )}
            <span className="ml-auto shrink-0 font-mono text-[11px] tabular-nums text-muted-foreground/70">
              {repo.last_activity_ts > 0 ? timeAgo(repo.last_activity_ts, now) : "—"}
            </span>
            <ChevronRight className="h-4 w-4 shrink-0 text-muted-foreground" />
          </div>
          <MetaLine repo={repo} />
        </Link>
        {/* A full-height 44px column, not a chip on a row of its own: it is the widest thumb target
            the card can afford without costing a single pixel of list height. */}
        <button
          type="button"
          aria-expanded={manage}
          aria-label={`manage ${repo.repo}`}
          onClick={() => setManage((v) => !v)}
          className={cn(
            "flex w-11 shrink-0 items-center justify-center border-l border-border/60 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground",
            manage && "bg-muted/60 text-foreground",
          )}
        >
          <Settings2 className="h-4 w-4" />
        </button>
      </div>

      {manage && (
        <div className="space-y-2.5 border-t border-border/60 bg-muted/30 px-3 py-2.5">
          <PoolPanel repo={repo.repo} pool={repo.pool} now={now} />
          {needsSetup && (
            <div className="flex flex-wrap items-center gap-2 border-t border-border/60 pt-2.5">
              <span className="text-xs text-muted-foreground">
                bn build/verify scripts are missing.
              </span>
              <SetupButton repo={repo.repo} />
            </div>
          )}
          <ForgeLinkRow links={repo.links} />
        </div>
      )}
    </Card>
  );
}

/** A filter chip with its live count. Thumb-sized, and `shrink-0` so the row scrolls sideways on a
 * phone instead of wrapping into a second band that pushes the first card off-screen. */
function FilterChip({
  active,
  label,
  count,
  onClick,
}: {
  active: boolean;
  label: string;
  count: number;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      aria-pressed={active}
      onClick={onClick}
      className={cn(
        "inline-flex h-9 shrink-0 items-center gap-1.5 rounded-full border px-3 text-sm transition-colors sm:h-8",
        active
          ? "border-primary/40 bg-primary/15 text-foreground"
          : "border-border bg-card text-muted-foreground hover:bg-accent hover:text-foreground",
      )}
    >
      {label}
      <span
        className={cn(
          "rounded-full px-1.5 text-xs tabular-nums",
          active ? "bg-primary/20 text-foreground" : "bg-muted text-muted-foreground",
        )}
      >
        {count}
      </span>
    </button>
  );
}

/** The board's search box: name-substring narrowing with a clear affordance, because emptying a
 * field by backspacing on a phone is far more work than tapping an ✕. */
function SearchField({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  return (
    <div className="relative">
      <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
      <Input
        type="search"
        inputMode="search"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder="Filter repos…"
        aria-label="filter repos by name"
        // `type="search"` for the searchbox role and the phone keyboard's Search key, minus
        // WebKit's own cancel glyph — left in, it draws a second ✕ on top of the one below.
        className="h-10 pl-8 pr-8 [&::-webkit-search-cancel-button]:appearance-none sm:h-8"
      />
      {value !== "" && (
        <button
          type="button"
          onClick={() => onChange("")}
          aria-label="clear repo filter"
          className="absolute right-1 top-1/2 inline-flex h-8 w-8 -translate-y-1/2 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
        >
          <X className="h-3.5 w-3.5" />
        </button>
      )}
    </div>
  );
}

/** The repo grid. One column until `lg`, not until `sm`. A repo card is a row of facts, and the
 * facts have a width: two columns at 640px gives each card ~300px, which truncates the meta line
 * that a single 640px column renders comfortably. Columns are worth having only once the viewport
 * can pay for them at ~400px a card. `items-start` because grid cells otherwise stretch to their
 * row's tallest sibling, so one card with its manage panel open grew a void under every card
 * beside it. */
const GRID = "grid grid-cols-1 items-start gap-2 sm:gap-3 lg:grid-cols-2 2xl:grid-cols-3";

/**
 * `/repos` — the local repo inventory (#495, Epic #488). Cards represent repos with local evidence:
 * a bare clone, a checked-out worktree, or a live/in-flight agent. Notes, managed config, and cost
 * data decorate those cards but do not create them. A repo bn has cloned but never worked in is a
 * 0-session card at the bottom, not a missing one. Drilling into a card opens that repo's session
 * list (`/repos/:repo`). Hydrated from `/api/repos`, patched by the fallback poll.
 *
 * Built mobile-first: the board narrows (search + chips) before it scrolls, each card's face is one
 * tap target with its mutating controls behind a disclosure, and the layout starts as a single
 * column and earns more only where the viewport has the width for them.
 *
 * The header used to carry an `N need setup` caption, and before that a button that started a setup
 * agent for every one of them — one click could put ten agents in the queue, which is a lot of
 * machine to commit from a header. It is a filter chip now: the same fact, except tapping it shows
 * you which repos it means instead of asking you to go find them.
 */
export function Repos() {
  const repos = useQuery(reposQuery());
  const now = useNow();
  const [filter, setFilter] = useState<RepoFilter>("all");
  const [query, setQuery] = useState("");
  const data = repos.data as PolicyReposView | undefined;
  const list = data?.repos ?? [];
  const counts = repoFilterCounts(list, query);
  const shown = filterRepos(list, filter, query);
  const narrowed = query.trim() !== "" || filter !== "all";

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="flex flex-none flex-col gap-2 border-b border-border px-3 py-2.5 sm:px-4">
        <div className="flex flex-wrap items-center gap-x-2 gap-y-2">
          <h1 className="font-display text-lg font-semibold tracking-tight text-foreground">
            Repos
          </h1>
          <span className="font-mono text-[11px] tabular-nums text-muted-foreground/70">
            {repos.isLoading
              ? "loading…"
              : `${list.length} repo${list.length === 1 ? "" : "s"} · live`}
          </span>
          {/* One field, re-ordered rather than duplicated: a full-width row of its own below the
              title on a phone, where a 40px target beats a cramped one, and inline at the end of
              the title row once there is width for it. Rendering a `sm:hidden` copy beside a
              `hidden sm:block` one would put two identical searchboxes in the accessibility tree. */}
          <div className="order-last w-full sm:order-none sm:ml-auto sm:w-56">
            <SearchField value={query} onChange={setQuery} />
          </div>
        </div>

        <div
          className="-mx-3 flex gap-2 overflow-x-auto px-3 sm:mx-0 sm:px-0"
          role="group"
          aria-label="filter repos"
        >
          <FilterChip
            active={filter === "all"}
            label="All"
            count={counts.all}
            onClick={() => setFilter("all")}
          />
          <FilterChip
            active={filter === "active"}
            label="Active"
            count={counts.active}
            onClick={() => setFilter("active")}
          />
          <FilterChip
            active={filter === "setup"}
            label="Needs setup"
            count={counts.setup}
            onClick={() => setFilter("setup")}
          />
          <FilterChip
            active={filter === "pooled"}
            label="Pooled"
            count={counts.pooled}
            onClick={() => setFilter("pooled")}
          />
        </div>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto px-3 py-3 sm:px-4 sm:py-4">
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

        {/* An empty inventory and an empty *filter* are different facts and must not render alike —
            the second is the board hiding rows that exist, so it names the way back. */}
        {!repos.isLoading && list.length > 0 && shown.length === 0 && (
          <div className="flex flex-col items-center gap-3 px-6 py-12 text-center">
            <h3 className="font-display text-base font-semibold">No repos match</h3>
            <p className="max-w-sm text-sm text-muted-foreground">
              {list.length} repo{list.length === 1 ? "" : "s"} here, none in this view.
            </p>
            <Button
              type="button"
              variant="secondary"
              size="sm"
              className={TOUCH}
              onClick={() => {
                setQuery("");
                setFilter("all");
              }}
            >
              Clear filters
            </Button>
          </div>
        )}

        {shown.length > 0 && (
          <div className={GRID}>
            {shown.map((r) => (
              <RepoCard key={r.repo} repo={r} now={now} />
            ))}
          </div>
        )}
      </div>

      {narrowed && shown.length > 0 && (
        <p className="flex-none border-t border-border px-3 py-1.5 font-mono text-[11px] tabular-nums text-muted-foreground/70 sm:px-4">
          showing {shown.length} of {list.length}
        </p>
      )}
    </div>
  );
}
