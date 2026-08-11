/**
 * Agents as *definitions*: the triggerable catalog (`/api/agents/catalog`) and the Markdown
 * roster (`/api/agent-defs`). Neither describes a running session — that is `LiveAgent` in
 * `types.ts`, keyed by harness session id.
 *
 * Split out of `types.ts`, which is the UI's largest file and sits at the file-size ratchet's
 * ceiling. Re-exported from `types.ts`, so importing from either path resolves the same types.
 */
// --- Triggerable-agent catalog (GET /api/agents/catalog .data; #557) ---

/** The manifest's explicit runnable agent kind, or a plain cron/native job. */
export type CatalogKind = "command" | "staff" | "hosted" | "specialist" | "cron";

/** One triggerable agent/job — `bn_core::action_center::CatalogItem` (#557). Everything the
 * automation can fire (declared crons + native jobs + the Markdown agent roster), with the
 * metadata needed to decide whether to re-trigger it. `job` is exactly what
 * `POST /api/cron/run {job}` fires;
 * specialists are install-only and are not emitted by this triggerable catalog. */
export interface CatalogItem {
  job: string;
  name: string;
  kind: CatalogKind;
  scope: string;
  declared: boolean;
  /** Neither a calendar nor startup trigger is declared. */
  on_demand: boolean;
  schedule?: string | null;
  description?: string | null;
  has_script: boolean;
  network: boolean;
  catch_up: boolean;
  startup: boolean;
  queue: boolean;
  retry_after_secs?: number | null;
  last_run?: number | null;
  last_exit?: number | null;
  ok?: boolean | null;
  runs: number;
  failures: number;
  skills?: string[];
  mcps?: string[];
  harness?: string | null;
}

/** GET /api/agents/catalog .data — the triggerable-agent catalog (`payload::catalog_data`, #557). */
export interface CatalogView {
  count: number;
  agents: CatalogItem[];
}

// --- Markdown agent roster (GET /api/agent-defs .data) ---------------------------------------

/** One declared argument a Markdown agent's body interpolates. */
export interface AgentDefArg {
  name: string;
  description?: string | null;
  required: boolean;
}

/** One Markdown agent definition — `bn_core::agents::AgentDef`, minus its prompt body.
 *
 * Distinct from `CatalogItem`: that is the *triggerable* catalog (what the automation can fire, so
 * it drops specialists and folds in plain cron jobs). This is the *definition* roster, so a
 * `specialist` is listed like anything else even though nothing can trigger it. */
export interface AgentDefRow {
  name: string;
  kind: CatalogKind;
  /** Tier the definition was discovered in: `branch` | `repo` | `global`. */
  scope: string;
  /** Absolute path to the source `.md` file. */
  path: string;
  description?: string | null;
  /** systemd `OnCalendar` string, or null for an unscheduled agent. */
  schedule?: string | null;
  startup: boolean;
  /** Neither a calendar nor a startup trigger is declared. */
  on_demand: boolean;
  /** Installable rather than runnable — it compiles into a native harness agent. */
  specialist: boolean;
  harness?: string | null;
  model?: string | null;
  run?: string | null;
  gather?: string | null;
  repo?: string | null;
  network: boolean;
  catch_up: boolean;
  queue: boolean;
  retry_after_secs?: number | null;
  skills: string[];
  mcps: string[];
  arguments: AgentDefArg[];
}

/** GET /api/agent-defs .data — the roster (`payload::agent_defs_data`). */
export interface AgentDefsView {
  count: number;
  agents: AgentDefRow[];
}

/** GET /api/agent-defs/:name .data — one definition plus its verbatim `.md` file, frontmatter
 * included (`payload::agent_def_data`). */
export interface AgentDefDetail {
  agent: AgentDefRow;
  source: string;
}
