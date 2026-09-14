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

// --- Agent roster CRUD (POST/PUT/DELETE /api/agent-defs; `bn_web::actions::agent_defs`) --------

/** The four explicit kinds an agent manifest can declare — `bn_core::agents::AgentKind`.
 *
 * Narrower than `CatalogKind`, which also carries `cron` for plain jobs that are not Markdown
 * agents at all and therefore can never be authored here. */
export const AGENT_KINDS = ["command", "staff", "hosted", "specialist"] as const;
export type AgentKind = (typeof AGENT_KINDS)[number];

/** The tiers a create may write into (`bn_core::agents::write::CREATE_SCOPES`).
 *
 * Only the global ones. The branch and repo tiers discovery also reads depend on a resolved
 * checkout, and bn-web serves a machine rather than a working copy — a "repo" tier here would mean
 * whichever repo the server happened to start in. Those files stay editable; they just cannot be
 * *created* from the dashboard. */
export const AGENT_DEF_SCOPES = ["global-personal", "global-shared"] as const;
export type AgentDefScope = (typeof AGENT_DEF_SCOPES)[number];

/** The Copilot/Claude native-agent re-install that rides on a specialist write.
 *
 * `null` rather than an all-zero report for every other kind: only a specialist compiles into a
 * native harness agent file, so "nothing to sync" is a different fact from "synced nothing", and
 * the UI says so. */
export interface AgentDefNativeSync {
  synced: boolean;
  harnesses: string[];
  created?: number;
  updated?: number;
  removed?: number;
  unchanged?: number;
  /** Present only on `synced: false` — the preflight that refused. */
  error?: string;
}

/** What a create/edit/delete returns: the file that moved, and the harness sync when there was
 * one. `remaining_scope` appears on a delete — non-null when a lower tier was unshadowed by it and
 * still answers to the name. */
export interface AgentDefMutation {
  agent: {
    name: string;
    scope: string;
    path: string;
    kind: AgentKind;
    specialist: boolean;
  };
  native: AgentDefNativeSync | null;
  remaining_scope?: string | null;
}

/** The body `POST /api/agent-defs` accepts (`bn_web::actions::agent_defs::AgentDefCreateBody`).
 *
 * `gather`/`sinks` are staff-only and `repo` is specialist-only — each kind's validator *forbids*
 * the fields belonging to the others, so sending a stray one is a `400` rather than a field the
 * server quietly drops. A specialist with an empty `body` is scaffolded server-side into a full
 * reviewer, the same file `bn agents new` writes. */
export interface AgentDefCreateBody {
  name: string;
  kind: AgentKind;
  description: string;
  scope?: AgentDefScope;
  gather?: string;
  sinks?: string[];
  repo?: string;
  body?: string;
  overwrite?: boolean;
}
