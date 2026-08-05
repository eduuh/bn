import { describe, expect, it } from "vitest";
import { cronNeedsAttention, scheduleLabel, sortCronJobs } from "./cron";
import type { CronJob } from "../api/types";

function job(over: Partial<CronJob>): CronJob {
  return {
    job: "j",
    scope: "global",
    declared: true,
    network: false,
    catch_up: false,
    has_script: false,
    runs: 0,
    failures: 0,
    ...over,
  };
}

describe("cronNeedsAttention", () => {
  it("flags a currently-failing job (last run exited non-zero)", () => {
    expect(cronNeedsAttention(job({ ok: false, last_exit: 1, runs: 1, failures: 1 }))).toBe(true);
  });

  it("flags a recovered job whose last run was ok but has past failures", () => {
    expect(cronNeedsAttention(job({ ok: true, runs: 5, failures: 2 }))).toBe(true);
  });

  it("does not flag a clean job with no failures", () => {
    expect(cronNeedsAttention(job({ ok: true, runs: 3, failures: 0 }))).toBe(false);
  });

  it("does not flag a never-run job", () => {
    expect(cronNeedsAttention(job({ ok: null, runs: 0, failures: 0 }))).toBe(false);
  });
});

describe("sortCronJobs", () => {
  it("orders currently-failing first, then recovered-with-failures, then healthy", () => {
    const jobs = [
      job({ job: "healthy", ok: true, runs: 1, failures: 0, last_run: 100 }),
      job({ job: "recovered", ok: true, runs: 5, failures: 2, last_run: 50 }),
      job({ job: "failing", ok: false, runs: 3, failures: 1, last_run: 10 }),
    ];
    expect(sortCronJobs(jobs).map((j) => j.job)).toEqual(["failing", "recovered", "healthy"]);
  });

  it("sorts a job with past failures above a healthy one even when the healthy ran more recently", () => {
    const jobs = [
      job({ job: "healthy-recent", ok: true, failures: 0, last_run: 999 }),
      job({ job: "recovered-old", ok: true, failures: 3, last_run: 1 }),
    ];
    expect(sortCronJobs(jobs)[0].job).toBe("recovered-old");
  });

  it("within the same rank, most-recently-run leads", () => {
    const jobs = [
      job({ job: "old", ok: true, failures: 0, last_run: 10 }),
      job({ job: "new", ok: true, failures: 0, last_run: 90 }),
    ];
    expect(sortCronJobs(jobs).map((j) => j.job)).toEqual(["new", "old"]);
  });

  it("does not mutate the input array", () => {
    const jobs = [job({ job: "a", last_run: 1 }), job({ job: "b", ok: false, last_run: 2 })];
    const copy = [...jobs];
    sortCronJobs(jobs);
    expect(jobs).toEqual(copy);
  });
});

describe("scheduleLabel", () => {
  it("reads on-demand for an undeclared, run-observed job (not 'not scheduled')", () => {
    expect(scheduleLabel(job({ declared: false }))).toBe("run-observed / on-demand");
  });

  it("reads 'no schedule' for a declared job with no OnCalendar spec", () => {
    expect(scheduleLabel(job({ declared: true, schedule: null }))).toBe("no schedule");
  });
});
