import { useSyncExternalStore } from "react";

/**
 * The experimental gate: optional surfaces stay hidden until `?experimental` names them.
 *
 * The dashboard grew fifteen destinations faster than any of them got tested end to end, so the
 * default surface is a small trusted set — Today + Cron — and the rest are opt-in *individually*,
 * so a tab can come back the moment it is trusted instead of waiting for the whole set:
 *
 *     /?experimental=fleet        Today + Fleet
 *     /?experimental=fleet,queue  Today + Cron + Fleet + Queue
 *     /?experimental=1            everything
 *     /?experimental=0            back to default tabs
 *
 * This is a *nav* gate, not a router gate: the routes still exist and still render, so bookmarks,
 * deep links and the `/cost` → `/insights?tab=cost` style redirects all keep working. What changes
 * is that nothing in the shell points at them.
 *
 * The flag is sticky. Gating on the param being present in the current URL would mean every in-app
 * `<NavLink>` had to carry it forward and a reload would drop you back to the bare shell, so the
 * resolved set is written to `localStorage` and stays until the param says otherwise.
 *
 * The URL is no longer the only way in: `/settings` writes the same set through
 * [`setExperimental`], so a tab can be flipped from a switch instead of by hand-editing a query
 * string you had to already know the syntax of.
 */
const STORAGE_KEY = "bn:experimental";
const PARAM = "experimental";

/** The marker for *every* tab, both in the param and in storage. */
const ALL = "*";

/** The same marker, for callers that need to recognise or write "everything" (`/settings`). */
export const EXPERIMENTAL_ALL = ALL;

/** `?experimental` with no value reads as *on* — that is what typing it by hand means. */
const ALL_VALUES = new Set(["", "1", "true", "yes", "on", "all", ALL]);
const NONE_VALUES = new Set(["0", "false", "no", "off", "none"]);

/**
 * Names that are not the tab's route slug. The rail says "Automation" and the palette says "Start
 * an agent", so typing either into the param should work rather than sending you to look up what
 * bn calls the route underneath.
 */
const ALIASES: Record<string, string> = {
  automation: "inbox",
  agents: "fleet",
  agent: "fleet",
  money: "cost",
  spend: "cost",
  machines: "nodes",
};

function canonical(key: string): string {
  const k = key.trim().toLowerCase().replace(/^\/+/, "");
  return ALIASES[k] ?? k;
}

/** The param's verdict for a query string, or `null` if it does not mention the flag at all. */
function fromSearch(search: string): Set<string> | null {
  const params = new URLSearchParams(search);
  if (!params.has(PARAM)) return null;
  const raw = (params.get(PARAM) ?? "").trim().toLowerCase();
  if (ALL_VALUES.has(raw)) return new Set([ALL]);
  if (NONE_VALUES.has(raw)) return new Set();
  // A comma/space list of tab keys. Unknown names are kept rather than rejected — they simply
  // match no tab, which is a quieter failure than silently dropping the whole param because one
  // entry was a typo.
  return new Set(
    raw
      .split(/[,\s]+/)
      .map(canonical)
      .filter(Boolean),
  );
}

function stored(): Set<string> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return new Set();
    return new Set(raw.split(",").filter(Boolean));
  } catch {
    // Private-mode Safari and friends throw on `localStorage`. The flag is a convenience, not a
    // correctness requirement — fall back to the bare shell rather than taking it down with us.
    return new Set();
  }
}

function persist(keys: Set<string>) {
  try {
    localStorage.setItem(STORAGE_KEY, [...keys].join(","));
  } catch {
    /* see `stored` — the in-memory set still governs this tab */
  }
}

function same(a: Set<string>, b: Set<string>): boolean {
  return a.size === b.size && [...a].every((k) => b.has(k));
}

/** What the shell asks: is *this* tab showing, and is anything showing at all? */
export interface Experimental {
  /** Whether one gated tab is revealed. `undefined` (an ungated tab) is always true. */
  has: (key?: string) => boolean;
  /** Whether any gated tab is revealed — drives the rail's group headings and the topbar chip. */
  any: boolean;
  /** The enabled keys, or `["*"]` for all. For the chip's tooltip. */
  keys: string[];
}

// Resolved at module load from the URL the page was opened with, so the very first paint already
// has the right rail: a flash of the full nav before an effect narrowed it would be worse than no
// gate at all.
let keys = fromSearch(window.location.search) ?? stored();
persist(keys);

let snapshot = build(keys);
const listeners = new Set<() => void>();

function build(set: Set<string>): Experimental {
  const all = set.has(ALL);
  return {
    has: (key?: string) => key == null || all || set.has(key),
    any: set.size > 0,
    keys: [...set],
  };
}

/**
 * Re-read the flag from a query string. Call this on every navigation: React Router owns the URL
 * after the first paint, so `window.location` alone would miss `?experimental=fleet` arriving via
 * an in-app link or a back button.
 */
export function syncExperimentalFromUrl(search: string) {
  const next = fromSearch(search);
  if (next === null || same(next, keys)) return;
  commit(next);
}

/**
 * Replace the revealed set from the UI — what the `/settings` switches call.
 *
 * It writes the same store the query param does, so flipping a switch re-renders the rail on the
 * spot and survives a reload, and `?experimental=` keeps overriding it on the next navigation.
 * Pass `[EXPERIMENTAL_ALL]` for everything and `[]` for the bare shell.
 */
export function setExperimental(next: Iterable<string>) {
  const set = new Set([...next].map(canonical).filter(Boolean));
  if (same(set, keys)) return;
  commit(set);
}

/** Adopt a resolved set: persist it, rebuild the snapshot, wake the subscribers. */
function commit(next: Set<string>) {
  keys = next;
  persist(next);
  snapshot = build(next);
  for (const l of listeners) l();
}

function subscribe(onChange: () => void): () => void {
  listeners.add(onChange);
  return () => listeners.delete(onChange);
}

/** Which experimental surfaces are showing. Re-renders when the set changes. */
export function useExperimental(): Experimental {
  return useSyncExternalStore(
    subscribe,
    () => snapshot,
    () => snapshot,
  );
}
