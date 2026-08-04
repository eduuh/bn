/**
 * How wide a route wants to be.
 *
 * The shell used to pin every route but `/console` to a centred `max-w-[1060px]` column. At 1440
 * that left 90px gutters; at 1920 it left 355px and the content visibly floated while the topbar
 * stayed full-bleed, so the page aligned to nothing. Worse, it forced one shape — a stack of
 * bordered cards on a document measure — onto sixteen screens that are not documents.
 *
 * Routes declare intent instead:
 *
 * - `full`     — full-bleed and self-scrolling. The console language: a dense multi-column tool
 *                surface that owns its own scrolling and fills the viewport.
 * - `standard` — fluid to a generous max, the page scrolls. For routes not yet converted; it still
 *                removes the float, because the max only bites on a very wide screen.
 * - `document` — a narrow measure for forms and prose, where a long line is the real enemy.
 *
 * A route moves to `full` when it has actually been rebuilt for it — listing one here that still
 * renders a card stack just stretches the cards.
 */
export type PageWidth = "full" | "standard" | "document";

/** Exact-path intents. */
const EXACT: Record<string, PageWidth> = {
  "/console": "full",
  "/today": "full",
  "/fleet": "full",

  // Not yet rebuilt for `full` — they still render a card stack, and a card stack in a full-bleed
  // container is strictly worse than one on a measure: the rows stretch to 1700px and the actions
  // end up further from their content, not closer. `standard` already removes the float, which was
  // the actual defect. Flip each to `full` in the commit that converts it.
  "/inbox": "full",
  "/insights": "full",
  "/repos": "full",
  "/notes": "standard",

  // Forms and prose: a wide measure would make these worse, not better.
  "/start": "document",
  "/research": "document",
  "/handoff": "document",
};

/** Prefix intents, longest-match-first, for the `:id` detail routes. */
const PREFIX: ReadonlyArray<readonly [string, PageWidth]> = [
  ["/agents/", "standard"],
  ["/repos/", "standard"],
  ["/cron/", "standard"],
];

export function pageWidth(pathname: string): PageWidth {
  const exact = EXACT[pathname];
  if (exact) return exact;
  for (const [prefix, width] of PREFIX) {
    if (pathname.startsWith(prefix)) return width;
  }
  return "standard";
}
