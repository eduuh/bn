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
