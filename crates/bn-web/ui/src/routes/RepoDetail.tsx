import { useState } from "react";
import { Link, useParams } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ArrowLeft,
  ArrowUpRight,
  Bot,
  ChevronRight,
  CircleDot,
  DollarSign,
  GitFork,
  Rocket,
  Users,
} from "lucide-react";
import {
  actionsQuery,
  fleetQuery,
  queryKeys,
  reposQuery,
  repoIssuesQuery,
} from "../api/client";
import { startRepoOrchestrator } from "../api/writes";
import type { ActionItem, LiveAgent, RepoIssue, RepoSession } from "../api/types";
import { useNow } from "../hooks/useNow";
import { timeAgo, fmtTokens, fmtAic, fmtUsd } from "../lib/format";
import { prHref, prLabel, jumpCommand } from "../lib/board";
import { findRepo, splitSessions, sessionTo, sessionTone, sessionStatusLabel } from "../lib/repos";
import { pushToast } from "../lib/toast";
import { CopyButton } from "../components/CopyButton";
import { StartAgentComposer } from "../components/StartAgentComposer";
import { toneVariant } from "../components/StateChip";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { Skeleton } from "../components/ui/skeleton";
import { cn } from "../lib/utils";

/** The left tone bar colour for a card's state. */
const BAR: Record<string, string> = {
  green: "bg-ok",
  red: "bg-urgent",
  amber: "bg-warn",
  grey: "bg-border",
  neutral: "bg-border",
};

/** A small metadata tag on a card's meta row. */
function Tag({ children, className }: { children: React.ReactNode; className?: string }) {
  return (
    <span
      className={cn(
        "inline-flex items-center rounded-md bg-muted px-1.5 py-0.5 text-xs text-muted-foreground",
        className,
      )}
    >
      {children}
    </span>
  );
}

/** A section header — an icon, a title, and a live count badge. */
function SectionHead({
  icon: Icon,
  title,
  count,
}: {
  icon: React.ComponentType<{ className?: string }>;
  title: string;
  count?: number;
}) {
  return (
    <div className="flex items-center gap-2">
      <Icon className="h-4 w-4 text-muted-foreground" />
      <h3 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">{title}</h3>
      {count != null && <Badge variant="muted">{count}</Badge>}
    </div>
  );
}

/** One session card in the drill-in list: status chip · branch · a meta strip (tokens · todos ·
 * blockers · PR · age). A live session taps through to its agent detail (`/agents/:id`); a note-only
 * branch stays a static glance card with a copy-to-jump tmux target — mirroring the Fleet card. */
function SessionCard({ s, now }: { s: RepoSession; now: number }) {
  const to = sessionTo(s);
  const tone = sessionTone(s);
  const href = prHref(s.pr ?? undefined);

  const inner = (
    <>
      <span
        className={cn("absolute inset-y-0 left-0 w-1", BAR[tone] ?? BAR.neutral)}
        aria-hidden="true"
      />
      <div className="flex items-center gap-2">
        <Badge variant={toneVariant(tone)} className="shrink-0">
          {sessionStatusLabel(s)}
        </Badge>
        <span className="flex-1" />
        {to && <ChevronRight className="h-4 w-4 shrink-0 text-muted-foreground" />}
      </div>
      <div className="mt-1 truncate font-mono text-xs text-muted-foreground">{s.branch}</div>
      <div className="mt-2 flex flex-wrap items-center gap-1.5">
        {s.output_tokens > 0 ? <Tag>{fmtTokens(s.output_tokens)} tok</Tag> : null}
        {s.open_todos > 0 && <Tag>{s.open_todos} todo</Tag>}
        {s.blockers > 0 && (
          <span className="inline-flex items-center rounded-md border border-urgent/30 bg-urgent-muted px-1.5 py-0.5 text-xs font-medium text-urgent">
            {s.blockers} blocked
          </span>
        )}
        {s.pr &&
          (href ? (
            <a
              href={href}
              target="_blank"
              rel="noreferrer"
              onClick={(e) => e.stopPropagation()}
              className="inline-flex items-center rounded-md bg-muted px-1.5 py-0.5 text-xs text-primary"
            >
              {prLabel(s.pr)}
            </a>
          ) : (
            <Tag>{prLabel(s.pr)}</Tag>
          ))}
        {s.last_activity_ts > 0 && (
          <Tag className="text-muted-foreground/70">{timeAgo(s.last_activity_ts, now)}</Tag>
        )}
      </div>
    </>
  );

  const surface = cn(
    "relative block overflow-hidden rounded-lg border border-border bg-card p-3 pl-4 text-card-foreground shadow-card transition-colors",
    to && "hover:border-border/80 hover:bg-accent/40",
  );

  if (to) {
    return (
      <Link to={to} className={surface}>
        {inner}
      </Link>
    );
  }
  return (
    <Card className={cn(surface, "block")}>
      {inner}
      {s.target && (
        <div className="mt-2">
          <CopyButton text={jumpCommand(s.target)} label={`⧉ ${s.target}`} />
        </div>
      )}
    </Card>
  );
}

/** Responsive card grid shared by both session tiers. */
function CardGrid({ children }: { children: React.ReactNode }) {
  return <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">{children}</div>;
}

/** A small "nothing here" placeholder used by the command-center sections. */
function Empty({ children }: { children: React.ReactNode }) {
  return (
    <Card className="px-4 py-6 text-center text-sm text-muted-foreground">{children}</Card>
  );
}

/** Build the synthetic action the Start-agent composer consumes for a "work on this issue" launch —
 * mirrors `bn_core::palette::issue_action` so a repo-issue start pre-fills the same fix-and-PR
 * prompt the command palette does. */
function issueAction(repo: string, i: RepoIssue): ActionItem {
  return {
    id: `issue:${i.url}`,
    type: "issue",
    title: `#${i.number} ${i.title}`,
    source: "github issue",
    severity: "info",
    repo,
    branch: null,
    link: i.url,
    detail: null,
    payload: { repo, issue: i.url },
    suggested_prompt: `Work on GitHub issue #${i.number} and open a PR that fixes it:\n\n${i.title.trim()}\n\n${i.url}\n\nFixes #${i.number}.`,
    default_urgency: "urgent",
    created: 0,
    stale: false,
  };
}

/**
 * Ask #1 — start (and see) the repo's main-branch orchestrator. Detects a live orchestrator from
 * the fleet (role `orchestrator`, this repo), so the button is a guarded one-click: it links
 * through to the running supervisor when one exists, or fires `POST /api/repos/:repo/orchestrate`
 * to spin one up. The server rejects a duplicate with a `409`, surfaced as a toast.
 */
function OrchestratorPanel({ repo, agents }: { repo: string; agents: LiveAgent[] }) {
  const qc = useQueryClient();
  const running = agents.find(
    (a) => a.repo.toLowerCase() === repo.toLowerCase() && a.role === "orchestrator",
  );
  const start = useMutation({
    mutationFn: () => startRepoOrchestrator(repo),
    onSuccess: () => {
      pushToast(`Starting an orchestrator for ${repo}…`, "success");
      void qc.invalidateQueries({ queryKey: queryKeys.fleet });
      void qc.invalidateQueries({ queryKey: queryKeys.orchestrate });
    },
    onError: (e) => pushToast(e instanceof Error ? e.message : "could not start", "error"),
  });

  return (
    <section className="space-y-3">
      <SectionHead icon={Bot} title="Orchestrator" />
      {running ? (
        <Link
          to={`/agents/${encodeURIComponent(running.session_id)}`}
          className="relative block overflow-hidden rounded-lg border border-border bg-card p-3 pl-4 text-card-foreground shadow-card transition-colors hover:border-border/80 hover:bg-accent/40"
        >
          <span className="absolute inset-y-0 left-0 w-1 bg-ok" aria-hidden="true" />
          <div className="flex items-center gap-2">
            <Badge variant="ok" className="shrink-0">
              running
            </Badge>
            <span className="text-sm">Orchestrator supervising this repo</span>
            <span className="flex-1" />
            <ChevronRight className="h-4 w-4 shrink-0 text-muted-foreground" />
          </div>
          <div className="mt-1 truncate font-mono text-xs text-muted-foreground">
            {running.target}
          </div>
        </Link>
      ) : (
        <Card className="space-y-3 p-4">
          <p className="text-sm text-muted-foreground">
            Start a main-branch orchestrator to spin up + supervise a fleet that drains this repo's
            open-issue backlog.
          </p>
          <Button
            type="button"
            className="w-full"
            disabled={start.isPending}
            onClick={() => {
              if (
                window.confirm(
                  `Start an orchestrator for ${repo}? It supervises a fleet against the open-issue backlog.`,
                )
              )
                start.mutate();
            }}
          >
            <Rocket className="h-4 w-4" /> Start orchestrator
          </Button>
        </Card>
      )}
    </section>
  );
}

/** Ask #2 — the repo's open GitHub issues, each one-click startable (reuses StartAgentComposer via
 * a synthetic issue action). Best-effort: an unauthed / offline server yields an empty list. */
function IssuesPanel({ repo, onStart }: { repo: string; onStart: (item: ActionItem) => void }) {
  const issues = useQuery(repoIssuesQuery(repo));
  const list = issues.data?.issues ?? [];
  return (
    <section className="space-y-3">
      <SectionHead icon={CircleDot} title="Open issues" count={list.length} />
      {issues.isLoading ? (
        <Skeleton className="h-16 w-full" />
      ) : list.length === 0 ? (
        <Empty>
          No open issues found for this repo (or `gh` is unavailable).
        </Empty>
      ) : (
        <ul className="space-y-2">
          {list.map((i) => (
            <li
              key={i.number}
              className="flex items-center gap-2 rounded-lg border border-border bg-card p-3 text-card-foreground shadow-card"
            >
              <a
                href={i.url}
                target="_blank"
                rel="noreferrer"
                className="min-w-0 flex-1 truncate text-sm hover:text-primary"
                title={i.title}
              >
                <span className="font-mono text-muted-foreground">#{i.number}</span> {i.title}
              </a>
              <Button
                type="button"
                variant="outline"
                size="sm"
                className="shrink-0"
                onClick={() => onStart(issueAction(repo, i))}
              >
                <Rocket className="h-3.5 w-3.5" /> Start
              </Button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** Ask #3 — a compact 30-day cost summary for this repo (AIC + $ benchmark) with a jump to the full
 * Cost view. The figures ride the same `/api/repos` rollup the header uses. */
function CostPanel({ aic, usd }: { aic: number | null; usd: number | null }) {
  return (
    <section className="space-y-3">
      <SectionHead icon={DollarSign} title="Cost (30d)" />
      <Card className="flex items-center gap-4 p-4">
        <div className="flex-1">
          <div className="text-lg font-semibold">{fmtAic(aic)} AIC</div>
          <div className="text-xs text-muted-foreground">{fmtUsd(usd)} API-equiv benchmark</div>
        </div>
        <Link
          to="/cost"
          className="inline-flex items-center gap-1 text-sm text-primary hover:underline"
        >
          View details <ArrowUpRight className="h-3.5 w-3.5" />
        </Link>
      </Card>
    </section>
  );
}

/** Ask #5 — cross-repo tasks that *touch* this repo: action-center items filed under another repo
 * (or global) whose title/detail/link references this repo by name. Surfaces shared / cross-cutting
 * work from the repo page without inventing a new data source. */
function CrossRepoPanel({ repo }: { repo: string }) {
  const actions = useQuery(actionsQuery());
  const key = repo.toLowerCase();
  const items = (actions.data?.actions ?? []).filter((a) => {
    const own = (a.repo ?? "").toLowerCase() === key;
    if (own) return false;
    const hay = `${a.title} ${a.detail ?? ""} ${a.link ?? ""}`.toLowerCase();
    return hay.includes(key);
  });
  if (items.length === 0) return null;
  return (
    <section className="space-y-3">
      <SectionHead icon={GitFork} title="Cross-repo tasks touching this repo" count={items.length} />
      <ul className="space-y-2">
        {items.map((a) => (
          <li
            key={a.id}
            className="rounded-lg border border-border bg-card p-3 text-card-foreground shadow-card"
          >
            <div className="flex items-center gap-2">
              {a.repo && (
                <Badge variant="muted" className="shrink-0 font-mono">
                  {a.repo}
                </Badge>
              )}
              {a.link ? (
                <a
                  href={a.link}
                  target="_blank"
                  rel="noreferrer"
                  className="min-w-0 flex-1 truncate text-sm hover:text-primary"
                  title={a.title}
                >
                  {a.title}
                </a>
              ) : (
                <span className="min-w-0 flex-1 truncate text-sm" title={a.title}>
                  {a.title}
                </span>
              )}
            </div>
            <div className="mt-1 text-xs text-muted-foreground">{a.source}</div>
          </li>
        ))}
      </ul>
    </section>
  );
}

/**
 * `/repos/:repo` — one repo's **command center** (#556): drive all work for a single repo from one
 * page. The rollup header, then Ask-by-Ask sections — start/see the repo's main-branch orchestrator,
 * its open GitHub issues (one-click start), a compact cost jump, the agents targeting it (its live
 * sessions), recent branches, and cross-repo tasks touching it. Composes the existing `/api/repos`,
 * `/api/fleet`, `/api/actions` folds plus the repo-scoped `/api/repos/:repo/issues` read.
 */
export function RepoDetail() {
  const { repo = "" } = useParams();
  const repos = useQuery(reposQuery());
  const fleet = useQuery(fleetQuery());
  const now = useNow();
  const rollup = findRepo(repos.data, repo);
  const { active, recent } = splitSessions(rollup?.sessions ?? []);
  const agents = fleet.data?.agents ?? [];
  const [composer, setComposer] = useState<ActionItem | null>(null);
  const plural = (n: number, w: string) => `${n} ${w}${n === 1 ? "" : "s"}`;

  return (
    <div className="mx-auto max-w-3xl space-y-5 pb-8">
      <Link
        to="/repos"
        className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
      >
        <ArrowLeft className="h-4 w-4" /> Repos
      </Link>

      {repos.isLoading && (
        <CardGrid>
          {[0, 1].map((i) => (
            <Card key={i} className="space-y-2 p-3 pl-4">
              <Skeleton className="h-4 w-1/2" />
              <Skeleton className="h-3 w-2/3" />
            </Card>
          ))}
        </CardGrid>
      )}

      {!repos.isLoading && !rollup && (
        <Card className="flex flex-col items-center gap-2 px-6 py-12 text-center">
          <h3 className="text-lg font-semibold">Repo not found</h3>
          <p className="text-sm text-muted-foreground">
            No repo <code className="font-mono">{repo}</code>. See{" "}
            <Link to="/repos" className="text-primary">
              all repos
            </Link>
            .
          </p>
        </Card>
      )}

      {rollup && (
        <>
          <header className="space-y-1">
            <h2 className="font-mono text-2xl font-semibold tracking-tight">{rollup.repo}</h2>
            <p className="text-sm text-muted-foreground">
              {rollup.active_agents} active · {plural(rollup.session_count, "session")} ·{" "}
              {plural(rollup.open_prs, "PR")}
              {rollup.aic != null && <> · {fmtAic(rollup.aic)} AIC</>}
            </p>
          </header>

          <OrchestratorPanel repo={rollup.repo} agents={agents} />

          <IssuesPanel repo={rollup.repo} onStart={setComposer} />

          <CostPanel aic={rollup.aic} usd={rollup.cost_usd} />

          {active.length > 0 && (
            <section className="space-y-3">
              <SectionHead icon={Users} title="Agents targeting this repo" count={active.length} />
              <CardGrid>
                {active.map((s) => (
                  <SessionCard key={s.branch} s={s} now={now} />
                ))}
              </CardGrid>
            </section>
          )}

          {recent.length > 0 && (
            <section className="space-y-3">
              <SectionHead icon={ChevronRight} title="Recent branches" count={recent.length} />
              <CardGrid>
                {recent.map((s) => (
                  <SessionCard key={s.branch} s={s} now={now} />
                ))}
              </CardGrid>
            </section>
          )}

          <CrossRepoPanel repo={rollup.repo} />

          {rollup.sessions.length === 0 && (
            <Empty>No sessions in this repo yet.</Empty>
          )}
        </>
      )}

      {composer && <StartAgentComposer item={composer} onClose={() => setComposer(null)} />}
    </div>
  );
}
