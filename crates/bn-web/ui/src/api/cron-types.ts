/** One manifest-owned agent schedule nested beneath the common `agent-scheduler` cron row. */
export interface AgentSchedule {
  job: string;
  kind: "command" | "staff" | "hosted" | "specialist";
  scope: string;
  schedule: string;
  description?: string | null;
  network: boolean;
  queue: boolean;
  startup: boolean;
  retry_after_secs: number;
  due: boolean;
  retry_at?: number | null;
  state: "due" | "retry" | "idle";
  last_run?: number | null;
  last_exit?: number | null;
  ok?: boolean | null;
  runs: number;
  failures: number;
  last_output?: string | null;
}

/** One cron job's whole-picture status, with manifest-owned agents nested on the scheduler row. */
export interface CronJob {
  job: string;
  scope: string;
  declared: boolean;
  schedule?: string | null;
  description?: string | null;
  network: boolean;
  catch_up: boolean;
  has_script: boolean;
  script_path?: string | null;
  last_run?: number | null;
  last_exit?: number | null;
  ok?: boolean | null;
  runs: number;
  failures: number;
  last_output?: string | null;
  agents?: AgentSchedule[];
}

/** GET /api/cron .data — the machine-wide cron registry. */
export interface CronView {
  state?: "not_enabled";
  machine?: string;
  message?: string;
  count: number;
  jobs: CronJob[];
}
