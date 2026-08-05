import { useState } from "react";
import { Link } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, CalendarClock, Clock, Loader2, RotateCw, Timer } from "lucide-react";
import { cronQuery, queryKeys } from "../api/client";
import { runCronJob } from "../api/writes";
import type { CronJob } from "../api/types";
import { useNow } from "../hooks/useNow";
import { timeAgo } from "../lib/format";
import { pushToast } from "../lib/toast";
import { cronNeedsAttention, scheduleLabel, sortCronJobs } from "../lib/cron";
import { Badge, type BadgeProps } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { Skeleton } from "../components/ui/skeleton";
import { cn } from "../lib/utils";
import { PageHead } from "../components/PageHead";

/** The health tone of a job's last run: green when it last exited 0, red when it failed, grey when
 * it has never run (declared/scripted but not yet observed). */
function healthTone(j: CronJob): { variant: BadgeProps["variant"]; bar: string; label: string } {
  if (j.last_run == null) return { variant: "muted", bar: "bg-border", label: "never run" };
  return j.ok
    ? { variant: "ok", bar: "bg-ok", label: "ok" }
    : { variant: "urgent", bar: "bg-urgent", label: `exit ${j.last_exit}` };
}

/** Re-run control (#538): fire `bn run <job>` — the same code path a scheduled timer runs —
 * confirming first, toasting the outcome, and invalidating the cron query so the refreshed last-run
 * health appears without a reload. */
function RerunButton({ job }: { job: string }) {
  const qc = useQueryClient();
  const mut = useMutation({
    mutationFn: () => runCronJob(job),
    onSuccess: () => {
      pushToast(`Ran ${job}`, "success");
      void qc.invalidateQueries({ queryKey: queryKeys.cron });
    },
    onError: (e) => {
      pushToast(`run ${job} failed: ${e.message}`, "error");
      // The run still recorded a failed outcome — refresh so its new last-run health shows.
      void qc.invalidateQueries({ queryKey: queryKeys.cron });
    },
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

/** One cron job row: what it's declared to do + its last-run health. */
function CronCard({ job, now }: { job: CronJob; now: number }) {
  const h = healthTone(job);
  const [showLog, setShowLog] = useState(false);
  // A job that recorded failures but whose *last* run was ok has recovered — flag it amber so it
  // still leads the list, without overstating it as currently-broken (that's the red exit chip).
  const recoveredFailures = job.failures > 0 && job.ok !== false;
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
          <h3 className="text-lg font-semibold">No cron jobs</h3>
          <p className="max-w-sm text-sm text-muted-foreground">
            No declared crons or recorded runs on this machine yet.
          </p>
        </Card>
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
