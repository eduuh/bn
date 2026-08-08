import { Link, useParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import {
  AlertTriangle,
  ArrowLeft,
  Bot,
  CalendarClock,
  Clock,
  ExternalLink,
  FileSearch,
  Loader2,
  RotateCw,
  Timer,
} from "lucide-react";
import { cronQuery, findingsQuery, queryKeys } from "../api/client";
import { runCronJob } from "../api/writes";
import type { CronJob } from "../api/types";
import { useAppMutation } from "../hooks/useAppMutation";
import { affects } from "../lib/invalidation";
import { useNow } from "../hooks/useNow";
import { timeAgo } from "../lib/format";
import { scheduleLabel } from "../lib/cron";
import { Badge, type BadgeProps } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { Skeleton } from "../components/ui/skeleton";
import { cn } from "../lib/utils";

/** The health tone of a job's last run: green when it last exited 0, red when it failed, grey when
 * it has never run. Mirrors the list card's tone so the two views agree. */
function healthTone(j: CronJob): { variant: BadgeProps["variant"]; bar: string; label: string } {
  if (j.agents?.some((agent) => agent.ok === false)) {
    return { variant: "urgent", bar: "bg-urgent", label: "agent failure" };
  }
  if (j.last_run == null) return { variant: "muted", bar: "bg-border", label: "never run" };
  return j.ok
    ? { variant: "ok", bar: "bg-ok", label: "ok" }
    : { variant: "urgent", bar: "bg-urgent", label: `exit ${j.last_exit ?? "?"}` };
}

/** A small metadata tag on a flag row. */
function Tag({ children }: { children: React.ReactNode }) {
  return (
    <span className="inline-flex items-center rounded-md bg-muted px-1.5 py-0.5 text-xs text-muted-foreground">
      {children}
    </span>
  );
}

/** Run-now control (#538): fire `bn run <job>` — the same code path a scheduled timer runs —
 * confirming first, toasting the outcome, and invalidating the cron + findings queries so the
 * refreshed last-run health and any newly-stored findings appear without a reload. */
function RunNowButton({ job }: { job: string }) {
  const mut = useAppMutation({
    mutationFn: () => runCronJob(job),
    invalidates: [...affects.schedule, queryKeys.findings(job)],
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
          <RotateCw className="h-4 w-4" /> run now
        </>
      )}
    </Button>
  );
}

/** The loading skeleton — header + body placeholders so the page never flashes empty. */
function LoadingSkeleton() {
  return (
    <div className="space-y-4" aria-hidden="true">
      <Card className="space-y-3 p-4">
        <Skeleton className="h-6 w-1/2" />
        <Skeleton className="h-4 w-2/3" />
      </Card>
      <Skeleton className="h-40 w-full rounded-lg" />
    </div>
  );
}

/**
 * `/cron/:id` — one cron job's rich detail hub (#579, from #575/#576). The rich-detail layout:
 * identity header (job/scope/health) · live last-run status + captured output tail · contextual
 * actions (run now) · related timeline — the durable findings the job *stored* (#576), newest-first
 * with each gathered item deep-linking out. Selects the job from the `/api/cron` fold client-side
 * (like RepoDetail) and reads `/api/findings?source=<job>` for its history.
 */
export function CronDetail() {
  const { id = "" } = useParams();
  const now = useNow();
  const cron = useQuery(cronQuery());
  const findings = useQuery(findingsQuery(id));
  const job = cron.data?.jobs.find((j) => j.job === id);
  const runs = findings.data?.runs ?? [];

  return (
    <div className="space-y-5 pb-8">
      <Link
        to="/cron"
        className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
      >
        <ArrowLeft className="h-4 w-4" /> Cron
      </Link>

      {cron.isLoading && <LoadingSkeleton />}

      {!cron.isLoading && !job && (
        <Card className="flex flex-col items-center gap-2 px-6 py-12 text-center">
          <AlertTriangle className="h-8 w-8 text-muted-foreground" />
          <h3 className="text-lg font-semibold">Cron job not found</h3>
          <p className="max-w-sm text-sm text-muted-foreground">
            No cron job <code className="font-mono">{id}</code> on this machine. See{" "}
            <Link to="/cron" className="text-primary">
              all cron jobs
            </Link>
            .
          </p>
        </Card>
      )}

      {job && (
        <>
          <Card className="relative overflow-hidden p-4 pl-5">
            <span
              className={cn("absolute inset-y-0 left-0 w-1", healthTone(job).bar)}
              aria-hidden="true"
            />
            <div className="flex flex-wrap items-center gap-2">
              <h2 className="font-mono text-2xl font-semibold tracking-tight">{job.job}</h2>
              <Badge variant={healthTone(job).variant}>{healthTone(job).label}</Badge>
              <Badge variant="muted">{job.scope}</Badge>
              <span className="flex-1" />
              <RunNowButton job={job.job} />
            </div>

            <div className="mt-3 flex flex-wrap items-center gap-x-4 gap-y-1.5 text-sm text-muted-foreground">
              {job.schedule ? (
                <span
                  className="inline-flex items-center gap-1 font-mono"
                  title="declared OnCalendar schedule"
                >
                  <CalendarClock className="h-4 w-4" /> {job.schedule}
                </span>
              ) : (
                <span className="inline-flex items-center gap-1">
                  <Timer className="h-4 w-4" /> {scheduleLabel(job)}
                </span>
              )}
              {job.last_run != null && (
                <span className="inline-flex items-center gap-1" title="last run">
                  <Clock className="h-4 w-4" /> ran {timeAgo(job.last_run, now)} ago
                </span>
              )}
              <span>
                {job.runs} run{job.runs === 1 ? "" : "s"}
                {job.failures > 0 && <span className="text-urgent"> · {job.failures} failed</span>}
              </span>
            </div>

            {job.description && (
              <p className="mt-3 text-sm text-foreground/90">{job.description}</p>
            )}

            {(job.declared || job.has_script || job.network || job.catch_up) && (
              <div className="mt-3 flex flex-wrap items-center gap-1.5">
                {job.declared && <Tag>registered</Tag>}
                {job.has_script && <Tag>script</Tag>}
                {job.network && <Tag>network</Tag>}
                {job.catch_up && <Tag>catch-up</Tag>}
                {job.script_path && <Tag>{job.script_path}</Tag>}
              </div>
            )}
          </Card>

          {job.agents && job.agents.length > 0 && (
            <section className="space-y-3">
              <div className="flex items-center gap-2">
                <h3 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">
                  Managed agent schedules
                </h3>
                <Badge variant="muted">{job.agents.length}</Badge>
              </div>
              <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                {job.agents.map((agent) => (
                  <Card key={agent.job} className="space-y-2 p-3">
                    <div className="flex flex-wrap items-center gap-2">
                      <Bot className="h-4 w-4 text-primary" aria-hidden="true" />
                      <span className="font-mono text-sm font-medium">{agent.job}</span>
                      <Badge variant="info">{agent.kind}</Badge>
                      <Badge variant={agent.due ? "warn" : "muted"}>{agent.state}</Badge>
                      {agent.ok === false && (
                        <Badge variant="urgent">exit {agent.last_exit ?? "?"}</Badge>
                      )}
                      <span className="flex-1" />
                      <RunNowButton job={agent.job} />
                    </div>
                    <p className="font-mono text-xs text-muted-foreground">{agent.schedule}</p>
                    {agent.description && (
                      <p className="text-xs text-foreground/80">{agent.description}</p>
                    )}
                    {agent.ok === false && agent.last_output && (
                      <pre className="max-h-40 overflow-auto rounded-md border border-urgent/30 bg-urgent-muted p-2 font-mono text-xs text-urgent">
                        {agent.last_output}
                      </pre>
                    )}
                  </Card>
                ))}
              </div>
            </section>
          )}

          {job.ok === false && (
            <Card className="border-urgent/30 bg-urgent-muted p-4 text-sm text-urgent">
              <div className="flex items-center gap-2 font-medium">
                <AlertTriangle className="h-4 w-4" />
                Last run failed
                {job.last_exit != null ? ` · exit ${job.last_exit}` : ""}
                {job.last_run != null ? ` · ${timeAgo(job.last_run, now)} ago` : ""}
              </div>
              {job.last_output && (
                <pre
                  className="mt-2 max-h-72 overflow-auto rounded-md border border-border bg-muted/60 p-2 font-mono text-xs text-muted-foreground"
                  aria-label={`${job.job} last output`}
                >
                  {job.last_output}
                </pre>
              )}
            </Card>
          )}

          <section className="space-y-3">
            <div className="flex items-center gap-2">
              <h3 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">
                Findings
              </h3>
              <Badge variant="muted">{runs.length}</Badge>
            </div>

            {findings.isLoading && <Skeleton className="h-24 w-full rounded-lg" />}

            {!findings.isLoading && runs.length === 0 && (
              <Card className="flex flex-col items-center gap-2 px-6 py-10 text-center text-sm text-muted-foreground">
                <FileSearch className="h-7 w-7" />
                No findings stored by this job yet.
              </Card>
            )}

            {runs.map((run) => (
              <Card key={run.run_id} className="space-y-2 p-3">
                <div className="flex items-center gap-2 text-xs text-muted-foreground">
                  <Clock className="h-3.5 w-3.5" />
                  <span title="gathered">{timeAgo(run.gathered_at, now)} ago</span>
                  <span className="flex-1" />
                  <Badge variant="muted">
                    {run.items.length} item{run.items.length === 1 ? "" : "s"}
                  </Badge>
                </div>
                {run.items.length > 0 && (
                  <ul className="space-y-1">
                    {run.items.map((it) => (
                      <li key={it.key}>
                        {it.url ? (
                          <a
                            href={it.url}
                            target="_blank"
                            rel="noreferrer"
                            className="inline-flex items-start gap-1 text-sm text-primary hover:underline"
                          >
                            <ExternalLink className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                            {it.text}
                          </a>
                        ) : (
                          <span className="text-sm text-foreground/90">{it.text}</span>
                        )}
                      </li>
                    ))}
                  </ul>
                )}
              </Card>
            ))}
          </section>
        </>
      )}
    </div>
  );
}
