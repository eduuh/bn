import type { AgentHealth } from "./types";

/** One session (branch) within a repo — a branch note joined to its live agent / dispatch. */
export interface RepoSession {
  repo: string;
  branch: string;
  status: string | null;
  pr: string | null;
  open_todos: number;
  blockers: number;
  worktree: string | null;
  path: string | null;
  last_activity_ts: number;
  session_id: string | null;
  health: AgentHealth | null;
  target: string | null;
  output_tokens: number;
  in_flight: boolean;
}

/** A repo's warm-worktree pool state (#497). */
export interface RepoPool {
  enabled: boolean;
  total: number;
  warm: number;
  leased: number;
  idle: number;
  last_warmed_ts: number | null;
  policy: RepoPoolPolicy;
  slots: RepoPoolSlot[];
}

export interface RepoPoolPolicy {
  min_ready: number;
  warm_windows: RepoPoolWarmWindow[];
}

export interface RepoPoolWarmWindow {
  name: string;
  at: string;
  fore: string;
}

/** One warm-pool slot, 1-indexed to match its on-disk `_pool/<n>` dir. */
export interface RepoPoolSlot {
  n: number;
  leased: boolean;
  warm: boolean;
  branch: string | null;
  holder: string | null;
  age_secs: number | null;
  warmed_ts: number | null;
}

export type RepoSetupStatus = "ready" | "needs-setup" | "unknown";
export type RepoVerifyMode = "command" | "scripts" | "missing";

export interface RepoSetupScript {
  name: string;
  exists: boolean;
  scope: "branch" | "project" | "repo" | "global" | null;
  path: string | null;
  repo_path: string;
  repo_owned: boolean;
  inherited: boolean;
  editable: boolean;
  needs_setup: boolean;
  scaffold: boolean;
}

export interface RepoSetupVerify {
  mode: RepoVerifyMode;
  command: string | null;
  jobs: string[];
}

export interface RepoSetup {
  status: RepoSetupStatus;
  needs_setup: boolean;
  reasons: string[];
  scripts: RepoSetupScript[];
  verify: RepoSetupVerify;
}

export interface RepoScriptView {
  repo: string;
  name: string;
  scope: "branch" | "project" | "repo" | "global" | null;
  path: string | null;
  repo_path: string;
  content: string;
  exists: boolean;
}

/** The largest pool a repo may be given — mirrors `bn_core::pool::MAX_POOL_SIZE`. */
export const MAX_POOL_SIZE = 16;

/** The smallest usable pool — mirrors `bn_core::pool::MIN_POOL_SIZE`. */
export const MIN_POOL_SIZE = 2;

export type Forge = "github" | "azure-devops" | "gitlab" | "unknown";

export interface ForgeLinks {
  forge: Forge;
  repo: string;
  issues: string | null;
  pulls: string | null;
}

export interface RepoRollup {
  repo: string;
  managed: boolean;
  active_agents: number;
  open_prs: number;
  session_count: number;
  last_activity_ts: number;
  aic: number | null;
  cost_usd: number | null;
  pool: RepoPool;
  setup: RepoSetup;
  links: ForgeLinks | null;
  sessions: RepoSession[];
}

/** GET /api/repos .data — the local repo inventory. */
export interface ReposView {
  repos: RepoRollup[];
  known: string[];
}
