/**
 * Today's suggestions — the proposal list and what accepting one put into the queue.
 *
 * Split out of `types.ts` (which is over the size ratchet and may not grow) when a suggestion
 * stopped being a standalone row: accepting one queues the work, so a suggestion now carries the
 * identity of what it produced.
 */

/** Where a suggested task sits in its review lifecycle — `bn_core::suggestions::SuggestionStatus`. */
export type SuggestionStatus = "suggested" | "accepted" | "dismissed" | "done";

/**
 * What accepting a suggestion put into the queue — `bn_core::suggestions::QueueView`.
 *
 * Joined live against the backlog on every read, so it is never a stale claim that work is running.
 * A `kind` of `enhance` means the entry is still resolving *where* the work happens; the work itself
 * is queued after that lands.
 */
export interface SuggestionQueue {
  repo: string;
  /** The backlog entry id. */
  entry: string;
  /** `ship` | `enhance` | … */
  kind: string;
  /** `pending` | `leased` | `done` | … */
  status: string;
  /** Whether it is still in flight. Branch on this, not on `status`. */
  live: boolean;
}

/** One proposed task synthesized from the cron findings — `bn_core::suggestions::Suggestion`
 * (#577): what to do (`title`), why (`rationale`), and where it came from (`source` + `source_key`,
 * plus an optional `url` to the finding). `id` is deterministic on `source`+`source_key`. */
export interface Suggestion {
  id: string;
  title: string;
  rationale: string;
  source: string;
  source_key: string;
  url: string;
  /** Epoch-seconds this suggestion was first created. */
  created_at: number;
  status: SuggestionStatus;
  /** The governing task accepting it minted (`t42`), absent until then. */
  task?: string;
  /** What accepting it put into the queue — `null` when nothing is queued for it. */
  queue?: SuggestionQueue | null;
}

/** GET /api/suggestions .data — Today's agent's suggested tasks, newest-first (`api::suggestions`). */
export interface SuggestionsView {
  count: number;
  suggestions: Suggestion[];
}
