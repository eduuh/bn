// Pure Repos-view logic (#495): derive the per-session card view-model (tone, label, detail link)
// and split a repo's sessions into the active vs. recent groups. Side-effect-free + unit-tested
// (`repos.test.ts`) so the Repos routes stay a thin render of these values, matching the Fleet/board
// split (fleet.ts / board.ts).

import type { ForgeLinks, RepoRollup, RepoSession, ReposView } from "../api/types";
import { healthTone, type ChipTone } from "./board";

/** One rendered forge link: where it goes and what to call it. */
export interface ForgeLink {
  href: string;
  label: string;
}

/** The forge links to render for a repo, in reading order (code · issues · PRs), naming each page
 * the way its own forge does — Azure DevOps has work items and "pull requests", GitLab has merge
 * requests. Only links the server actually resolved appear, so an unknown forge yields just its
 * code page and a repo with no clone here yields nothing at all. */
export function forgeLinks(links: ForgeLinks | null | undefined): ForgeLink[] {
  if (!links) return [];
  const named = {
    github: { repo: "GitHub", issues: "issues", pulls: "pulls" },
    "azure-devops": { repo: "Azure DevOps", issues: "work items", pulls: "PRs" },
    gitlab: { repo: "GitLab", issues: "issues", pulls: "merge requests" },
    unknown: { repo: "remote", issues: "issues", pulls: "PRs" },
  }[links.forge];
  const out: ForgeLink[] = [{ href: links.repo, label: named.repo }];
  if (links.issues) out.push({ href: links.issues, label: named.issues });
  if (links.pulls) out.push({ href: links.pulls, label: named.pulls });
  return out;
}

/** The chip tone for a session: its live-agent health when running, in-flight green for a dispatch
 * with no joined agent yet, else neutral grey for a note-only branch. */
export function sessionTone(s: RepoSession): ChipTone {
  if (s.health) return healthTone(s.health);
  if (s.in_flight) return "green";
  return "grey";
}

/** A short status chip label: the live health, an in-flight orchestrate pane-state, or the note
 * status; falls back to a generic word so a card always has a label. */
export function sessionStatusLabel(s: RepoSession): string {
  if (s.health) return s.health;
  if (s.in_flight) return s.status ?? "in-flight";
  return s.status ?? "note";
}

/** The detail link for a session — the agent view when a live session backs it, else `null` (a
 * note-only branch has no live detail; the card stays a static glance with a copy-jump target). */
export function sessionTo(s: RepoSession): string | null {
  return s.session_id ? `/agents/${encodeURIComponent(s.session_id)}` : null;
}

/** Split a repo's sessions into the active (live / in-flight) set and the recent (note-only) rest,
 * preserving the server's in-flight-first, most-recent ordering. */
export function splitSessions(sessions: RepoSession[]): {
  active: RepoSession[];
  recent: RepoSession[];
} {
  return {
    active: sessions.filter((s) => s.in_flight),
    recent: sessions.filter((s) => !s.in_flight),
  };
}

/** Find one repo's rollup by name (case-insensitive) in the view. */
export function findRepo(view: ReposView | undefined, repo: string): RepoRollup | undefined {
  return view?.repos.find((r) => r.repo.toLowerCase() === repo.toLowerCase());
}

// --- Board filtering (#495) ---
//
// The board is a phone surface first, and a phone shows about four cards at a time. With a dozen
// repos cloned that is three screens of scrolling to answer "which one is busy" — so the board
// narrows before it scrolls. The predicates live here, pure and tested, because the route is meant
// to be a render of these values rather than a place where filtering logic accumulates.

/** The board's narrowing modes: everything, repos with a live agent, repos whose bn scripts are
 * missing, repos with a warm pool configured. */
export type RepoFilter = "all" | "active" | "setup" | "pooled";

/** The shape the filters actually read. Structural rather than `RepoRollup` so the route can pass
 * its pool-policy-widened rollup without a cast. */
export interface FilterableRepo {
  repo: string;
  active_agents: number;
  setup?: { needs_setup: boolean } | null;
  pool: { enabled: boolean };
}

/** Does this repo belong in `filter`? */
export function matchesFilter(repo: FilterableRepo, filter: RepoFilter): boolean {
  switch (filter) {
    case "active":
      return repo.active_agents > 0;
    case "setup":
      return repo.setup?.needs_setup === true;
    case "pooled":
      return repo.pool.enabled;
    default:
      return true;
  }
}

/** Case-insensitive substring match on the repo name. Substring rather than prefix because the
 * names people type are usually the tail (`-web`), not the owner. An all-whitespace query matches
 * everything, so a stray space never empties the board. */
export function matchesQuery(repo: FilterableRepo, query: string): boolean {
  const q = query.trim().toLowerCase();
  return q === "" || repo.repo.toLowerCase().includes(q);
}

/** The board's visible repos: the server's activity ordering, narrowed by chip and search box. */
export function filterRepos<T extends FilterableRepo>(
  repos: T[],
  filter: RepoFilter,
  query: string,
): T[] {
  return repos.filter((r) => matchesFilter(r, filter) && matchesQuery(r, query));
}

/** The live count behind each chip. Counted against the search results, not the whole inventory, so
 * a chip never advertises rows that the current search has already excluded. */
export function repoFilterCounts(
  repos: FilterableRepo[],
  query: string,
): Record<RepoFilter, number> {
  const searched = repos.filter((r) => matchesQuery(r, query));
  return {
    all: searched.length,
    active: searched.filter((r) => matchesFilter(r, "active")).length,
    setup: searched.filter((r) => matchesFilter(r, "setup")).length,
    pooled: searched.filter((r) => matchesFilter(r, "pooled")).length,
  };
}
