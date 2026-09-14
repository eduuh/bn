import { useMemo, useState } from "react";
import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { AlertTriangle, Bot, CalendarClock, Clock, Loader2, RotateCw, Timer } from "lucide-react";
import { cronQuery } from "../api/client";
import { runCronJob } from "../api/writes";
import type { AgentSchedule, CronJob } from "../api/types";
import { useAppMutation } from "../hooks/useAppMutation";
import { affects } from "../lib/invalidation";
import { useNow } from "../hooks/useNow";
import { timeAgo } from "../lib/format";
import { cronNeedsAttention, scheduleLabel, sortCronJobs } from "../lib/cron";
import { Badge, type BadgeProps } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import { Input } from "../components/ui/input";
import { Skeleton } from "../components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "../components/ui/table";
import { cn } from "../lib/utils";
import { PageHead } from "../components/PageHead";

/** The health tone of a job's last run: green when it last exited 0, red when it failed, grey when
 * it has never run (declared/scripted but not yet observed). */
function healthTone(j: CronJob): { variant: BadgeProps["variant"]; bar: string; label: string } {
  if (j.agents?.some((agent) => agent.ok === false)) {
    return { variant: "urgent", bar: "bg-urgent", label: "agent failure" };
  }
  if (j.last_run == null) return { variant: "muted", bar: "bg-border", label: "never run" };
  return j.ok
    ? { variant: "ok", bar: "bg-ok", label: "ok" }
    : { variant: "urgent", bar: "bg-urgent", label: `exit ${j.last_exit}` };
}

function agentStateVariant(agent: AgentSchedule): BadgeProps["variant"] {
  if (agent.ok === false) return "urgent";
  if (agent.due) return "warn";
  return "muted";
}

function agentStateLabel(agent: AgentSchedule, now: number): string {
  if (agent.retry_at != null) {
    return `retry in ${Math.max(1, Math.ceil((agent.retry_at - now) / 60))}m`;
  }
  return agent.state;
}

function sortAgentSchedules(agents: AgentSchedule[]): AgentSchedule[] {
  const rank = (agent: AgentSchedule) =>
    agent.ok === false ? 0 : agent.due ? 1 : agent.retry_at != null ? 2 : 3;
  const lastRun = (agent: AgentSchedule) => agent.last_run ?? 0;
  return [...agents].sort(
    (a, b) => rank(a) - rank(b) || lastRun(b) - lastRun(a) || a.job.localeCompare(b.job),
  );
}

/** Re-run control (#538): fire `bn run <job>` — the same code path a scheduled timer runs —
 * confirming first, toasting the outcome, and invalidating the schedule surfaces so the refreshed
 * last-run health appears without a reload. A failed run still recorded an outcome, which is
 * exactly why the reconcile happens on settle rather than only on success. */
function RerunButton({ job }: { job: string }) {
  const mut = useAppMutation({
    mutationFn: () => runCronJob(job),
    invalidates: affects.schedule,
    toast: { success: `Ran ${job}`, error: `run ${job} failed` },
  });
  return (
    <Button
      variant="secondary"
      size="sm"
      disabled={mut.isPending}
      title={`Run \`bn run ${job}\` now`}
      onClick={() => {
        if (window.confirm(`Run ${job} now? Fires \`bn run ${job}\`.`)) mut.mutate();
      }}
    >
      {mut.isPending ? (
        <>
          <Loader2 className="h-4 w-4 animate-spin" /> running…
        </>
      ) : (
        <>
          <RotateCw className="h-4 w-4" /> re-run
        </>
      )}
    </Button>
  );
}

/** A small metadata tag on the card's flag row. */
function Tag({ children }: { children: React.ReactNode }) {
  return (
    <span className="inline-flex items-center rounded-md bg-muted px-1.5 py-0.5 text-xs text-muted-foreground">
      {children}
    </span>
  );
}

/** One manifest-owned schedule, rendered as a child of the common scheduler rather than a second
 * top-level cron. */
function AgentScheduleRow({ agent, now }: { agent: AgentSchedule; now: number }) {
  const state = agentStateLabel(agent, now);
  return (
    <div className="rounded-md border border-border bg-background/60 p-2.5">
      <div className="flex flex-wrap items-center gap-2">
        <Bot className="h-4 w-4 text-primary" aria-hidden="true" />
        <span className="font-mono text-sm font-medium text-foreground">{agent.job}</span>
        <Badge variant="info">{agent.kind}</Badge>
        <Badge variant={agent.due ? "warn" : "muted"}>{state}</Badge>
        {agent.ok === false && <Badge variant="urgent">exit {agent.last_exit ?? "?"}</Badge>}
        <span className="flex-1" />
        <RerunButton job={agent.job} />
      </div>
      <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
        <span className="inline-flex items-center gap-1 font-mono">
          <CalendarClock className="h-3.5 w-3.5" /> {agent.schedule}
        </span>
        {agent.last_run != null ? (
          <span className="inline-flex items-center gap-1">
            <Clock className="h-3.5 w-3.5" /> ran {timeAgo(agent.last_run, now)} ago
          </span>
        ) : (
          <span>never run</span>
        )}
        <span>{Math.round(agent.retry_after_secs / 60)}m failure backoff</span>
      </div>
      {agent.description && (
        <p className="mt-1.5 text-xs text-foreground/80">{agent.description}</p>
      )}
    </div>
  );
}

/** Manifest-owned Markdown agents get their own above-the-fold schedule strip, even though they
 * are still nested under `agent-scheduler` below for the raw cron registry view. */
function AgentSchedulesSection({ agents, now }: { agents: AgentSchedule[]; now: number }) {
  const [query, setQuery] = useState("");
  const sorted = useMemo(() => sortAgentSchedules(agents), [agents]);
  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return sorted;
    return sorted.filter((agent) =>
      [
        agent.job,
        agent.kind,
        agent.scope,
        agent.schedule,
        agent.description ?? "",
        agentStateLabel(agent, now),
        agent.ok === false ? "failed" : "",
        agent.due ? "due" : "",
      ]
        .join(" ")
        .toLowerCase()
        .includes(needle),
    );
  }, [now, query, sorted]);
  if (agents.length === 0) return null;
  const due = agents.filter((agent) => agent.due).length;
  const failing = agents.filter((agent) => agent.ok === false).length;
  return (
    <Card>
      <CardHeader className="gap-3">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div className="space-y-1">
            <CardTitle>Agent schedules</CardTitle>
            <CardDescription>
              {agents.length} Markdown agent{agents.length === 1 ? "" : "s"} managed by
              agent-scheduler
              {due > 0 ? ` · ${due} due` : ""}
              {failing > 0 ? ` · ${failing} failing` : ""}
            </CardDescription>
          </div>
          <Input
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="Filter agents, schedules, state…"
            className="w-full sm:w-72"
            aria-label="Filter scheduled agents"
          />
        </div>
      </CardHeader>
      <CardContent className="p-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Agent</TableHead>
              <TableHead>State</TableHead>
              <TableHead>Schedule</TableHead>
              <TableHead>Last run</TableHead>
              <TableHead className="text-right">Action</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {filtered.length === 0 ? (
              <TableRow>
                <TableCell colSpan={5} className="h-24 text-center text-muted-foreground">
                  No scheduled agents match this filter.
                </TableCell>
              </TableRow>
            ) : (
              filtered.map((agent) => (
                <TableRow key={agent.job}>
                  <TableCell>
                    <div className="space-y-1">
                      <div className="flex flex-wrap items-center gap-2">
                        <span className="font-mono font-medium text-foreground">{agent.job}</span>
                        <Badge variant="info">{agent.kind}</Badge>
                        {agent.ok === false && (
                          <Badge variant="urgent">exit {agent.last_exit ?? "?"}</Badge>
                        )}
                      </div>
                      {agent.description && (
                        <p className="max-w-xl text-xs text-muted-foreground">
                          {agent.description}
                        </p>
                      )}
                      {agent.ok === false && agent.last_output && (
                        <pre className="mt-2 max-h-36 max-w-xl overflow-auto rounded-md border border-urgent/30 bg-urgent-muted p-2 font-mono text-xs text-urgent">
                          {agent.last_output}
                        </pre>
                      )}
                    </div>
                  </TableCell>
                  <TableCell>
                    <Badge variant={agentStateVariant(agent)}>{agentStateLabel(agent, now)}</Badge>
                  </TableCell>
                  <TableCell className="font-mono text-xs">{agent.schedule}</TableCell>
                  <TableCell className="text-muted-foreground">
                    {agent.last_run != null ? `${timeAgo(agent.last_run, now)} ago` : "never"}
                  </TableCell>
                  <TableCell className="text-right">
                    <RerunButton job={agent.job} />
                  </TableCell>
                </TableRow>
              ))
            )}
          </TableBody>
        </Table>
      </CardContent>
    </Card>
  );
}

/** One cron job row: what it's declared to do + its last-run health. */
function CronCard({ job, now }: { job: CronJob; now: number }) {
  const h = healthTone(job);
  const [showLog, setShowLog] = useState(false);
  // A job that recorded failures but whose *last* run was ok has recovered — flag it amber so it
  // still leads the list, without overstating it as currently-broken (that's the red exit chip).
  const recoveredFailures =
    (job.failures > 0 || (job.agents?.some((agent) => agent.failures > 0) ?? false)) &&
    h.variant !== "urgent";
  return (
    <Card
      className="relative overflow-hidden p-3 pl-4"
      role="article"
      aria-label={`cron ${job.job}`}
    >
      <span className={cn("absolute inset-y-0 left-0 w-1", h.bar)} aria-hidden="true" />
      <div className="flex flex-wrap items-center gap-2">
        <Link
          to={`/cron/${encodeURIComponent(job.job)}`}
          className="truncate font-mono text-sm font-semibold text-foreground hover:text-primary hover:underline"
        >
          {job.job}
        </Link>
        <Badge variant={h.variant}>{h.label}</Badge>
        {recoveredFailures && (
          <Badge
            variant="warn"
            title={`${job.failures} recorded failure${job.failures === 1 ? "" : "s"}`}
          >
            <AlertTriangle className="h-3 w-3" /> {job.failures} failed
          </Badge>
        )}
        <span className="flex-1" />
        <Badge variant="muted">{job.scope}</Badge>
        <RerunButton job={job.job} />
      </div>

      <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
        {job.schedule ? (
          <span
            className="inline-flex items-center gap-1 font-mono"
            title="declared OnCalendar schedule"
          >
            <CalendarClock className="h-3.5 w-3.5" /> {job.schedule}
          </span>
        ) : (
          <span
            className="inline-flex items-center gap-1"
            title={
              job.declared
                ? "declared but not scheduled"
                : "no declared schedule — fired on demand / observed from runs"
            }
          >
            <Timer className="h-3.5 w-3.5" /> {scheduleLabel(job)}
          </span>
        )}
        {job.last_run != null && (
          <span className="inline-flex items-center gap-1" title="last run">
            <Clock className="h-3.5 w-3.5" /> ran {timeAgo(job.last_run, now)} ago
          </span>
        )}
        {job.runs > 0 && (
          <span>
            {job.runs} run{job.runs === 1 ? "" : "s"}
            {job.failures > 0 && <span className="text-urgent"> · {job.failures} failed</span>}
          </span>
        )}
      </div>

      {job.ok === false && (
        <div className="mt-2 rounded-md border border-urgent/30 bg-urgent-muted p-2 text-xs text-urgent">
          Last run failed
          {job.last_exit != null ? ` · exit ${job.last_exit}` : ""}
          {job.last_run != null ? ` · ${timeAgo(job.last_run, now)} ago` : ""}
          {job.last_output && (
            <>
              {" · "}
              <button
                type="button"
                className="font-medium underline underline-offset-2 hover:no-underline"
                aria-expanded={showLog}
                onClick={() => setShowLog((v) => !v)}
              >
                {showLog ? "hide output" : "show output"}
              </button>
            </>
          )}
          {job.last_output && showLog && (
            <pre
              className="mt-2 max-h-64 overflow-auto rounded-md border border-border bg-muted/60 p-2 font-mono text-xs text-muted-foreground"
              aria-label={`${job.job} last output`}
            >
              {job.last_output}
            </pre>
          )}
        </div>
      )}

      {job.description && <p className="mt-2 text-sm text-foreground/90">{job.description}</p>}

      {(job.declared || job.has_script || job.network || job.catch_up) && (
        <div className="mt-2 flex flex-wrap items-center gap-1.5">
          {job.declared && <Tag>registered</Tag>}
          {job.has_script && <Tag>script</Tag>}
          {job.network && <Tag>network</Tag>}
          {job.catch_up && <Tag>catch-up</Tag>}
        </div>
      )}

      {job.agents && job.agents.length > 0 && (
        <div className="mt-3 space-y-2 border-l-2 border-primary/30 pl-3">
          <p className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
            Managed agent schedules
          </p>
          {job.agents.map((agent) => (
            <AgentScheduleRow key={agent.job} agent={agent} now={now} />
          ))}
        </div>
      )}
    </Card>
  );
}

/** The loading skeleton — placeholder cards so the page never flashes an empty body (#544). */
function LoadingSkeleton() {
  return (
    <div className="grid grid-cols-1 gap-3 sm:grid-cols-2" aria-hidden="true">
      {[0, 1, 2, 3].map((i) => (
        <Card key={i} className="space-y-2 p-3 pl-4">
          <Skeleton className="h-4 w-1/2" />
          <Skeleton className="h-3 w-2/3" />
          <Skeleton className="h-3 w-1/3" />
        </Card>
      ))}
    </div>
  );
}

/**
 * `/cron` — the machine's cron registry (#483). Every declared cron (`crons.toml`) joined with its
 * last-run health from the event log, plus run-only orphans. Read-only: the schedule + last-run
 * status the `bn cron` dashboard shows, surfaced for the phone. Failing jobs lead.
 */
export function Cron() {
  const now = useNow();
  const cron = useQuery(cronQuery());
  const jobs = cron.data?.jobs ?? [];
  const agentSchedules = jobs.flatMap((job) => job.agents ?? []);
  // Failing/at-risk first (currently-failing, then recovered-with-past-failures), then most-recently
  // -run, then declared-but-idle.
  const sorted = sortCronJobs(jobs);
  const attention = jobs.filter(cronNeedsAttention).length;

  return (
    <div className="space-y-5 pb-8">
      <PageHead
        title="Cron"
        sub={
          cron.isLoading
            ? "loading…"
            : `${jobs.length} job${jobs.length === 1 ? "" : "s"}${
                attention > 0 ? ` · ${attention} need attention` : ""
              }`
        }
      />

      {cron.isLoading && <LoadingSkeleton />}

      {cron.isError && (
        <Card className="flex items-center gap-2 border-urgent/30 bg-urgent-muted p-4 text-sm text-urgent">
          <AlertTriangle className="h-4 w-4" />
          Could not load the cron registry.
        </Card>
      )}

      {!cron.isLoading && !cron.isError && jobs.length === 0 && (
        <Card className="flex flex-col items-center gap-2 px-6 py-12 text-center">
          <CalendarClock className="h-8 w-8 text-muted-foreground" />
          <h3 className="text-lg font-semibold">
            {cron.data?.state === "not_enabled" ? "Cron is not enabled" : "No cron jobs"}
          </h3>
          <p className="max-w-sm text-sm text-muted-foreground">
            {cron.data?.state === "not_enabled"
              ? (cron.data.message ?? "This machine is not configured to run scheduled jobs.")
              : "No declared crons or recorded runs on this machine yet."}
          </p>
        </Card>
      )}

      {!cron.isLoading && !cron.isError && (
        <AgentSchedulesSection agents={agentSchedules} now={now} />
      )}

      {/* `items-start`: grid cells stretch to their row's tallest sibling by default, so a
            three-line job card sitting next to a nine-line one grew a void under its content
            rather than staying its own size. */}
      {sorted.length > 0 && (
        <div className="grid grid-cols-1 items-start gap-3 sm:grid-cols-2">
          {sorted.map((j) => (
            <CronCard key={`${j.scope}:${j.job}`} job={j} now={now} />
          ))}
        </div>
      )}
    </div>
  );
}
