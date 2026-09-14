/**
 * Today's suggestions — the proposal list and the task accepting one minted.
 *
 * Split out of `types.ts` (which is over the size ratchet and may not grow) when a suggestion
 * stopped being a standalone row, so a suggestion now carries the identity of what it produced.
 */

/** Where a suggested task sits in its review lifecycle — `bn_core::suggestions::SuggestionStatus`. */
export type SuggestionStatus = "suggested" | "accepted" | "dismissed" | "done";

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
}

/** GET /api/suggestions .data — Today's agent's suggested tasks, newest-first (`api::suggestions`). */
export interface SuggestionsView {
  count: number;
  suggestions: Suggestion[];
}
