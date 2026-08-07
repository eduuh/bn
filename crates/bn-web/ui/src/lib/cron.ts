// Pure cron-view logic for the Cron page (#483/#531). Side-effect-free + unit-tested (`cron.test.ts`)
// so the mobile Cron view stays a thin render of these values: order the jobs so anything worrying
// leads, and label an undeclared, run-observed job as on-demand rather than "not scheduled".

import type { CronJob } from "../api/types";

/** Whether a job is worrying — its last run failed, or it carries any recorded failures (even if it
 * has since recovered). These lead the list regardless of last-run status. */
export function cronNeedsAttention(j: CronJob): boolean {
  return (
    j.ok === false ||
    j.failures > 0 ||
    (j.agents?.some((agent) => agent.ok === false || agent.failures > 0) ?? false)
  );
}

/** Order for the Cron list: currently-failing first (last run exited non-zero), then jobs with past
 * failures that have since recovered, then healthy — within each rank, most-recently-run first. */
export function sortCronJobs(jobs: CronJob[]): CronJob[] {
  const currentFailure = (j: CronJob) =>
    j.ok === false || (j.agents?.some((agent) => agent.ok === false) ?? false);
  const pastFailure = (j: CronJob) =>
    j.failures > 0 || (j.agents?.some((agent) => agent.failures > 0) ?? false);
  const rank = (j: CronJob) => (currentFailure(j) ? 0 : pastFailure(j) ? 1 : 2);
  const lastRun = (j: CronJob) =>
    Math.max(j.last_run ?? 0, ...(j.agents?.map((agent) => agent.last_run ?? 0) ?? [0]));
  return [...jobs].sort((a, b) => rank(a) - rank(b) || lastRun(b) - lastRun(a));
}

/** The scheduling label for a job with no declared OnCalendar spec: a declared-but-unscheduled job
 * reads "no schedule"; an undeclared, run-observed job is on-demand — not "not scheduled". */
export function scheduleLabel(j: CronJob): string {
  return j.declared ? "no schedule" : "run-observed / on-demand";
}
