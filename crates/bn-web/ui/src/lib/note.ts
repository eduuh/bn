// Pure note-rendering policy for the agent session view (#484). Given a `NoteDetail` (the branch
// note the server resolved for a live agent), derive the presentational pieces the view renders:
// the prominent PR link, the deduped resource references, the captured session scripts, and the
// structured sections in display order. Side-effect-free so the render stays a thin projection and
// every derivation is unit-tested (`note.test.ts`).

import type { NoteDetail, NoteFront, NoteItem } from "../api/types";
import { prHref, prLabel } from "./board";

/** A single http(s) URL matcher. Stops at whitespace and common wrapping/trailing punctuation so a
 * URL inside prose ("see https://x/y.") doesn't swallow the sentence period or a closing paren. */
const URL_RE = /https?:\/\/[^\s<>()[\]"'`]+/g;

/** Every http(s) URL in `text`, in order, with trailing sentence punctuation trimmed. */
export function extractUrls(text: string | null | undefined): string[] {
  if (!text) return [];
  const out: string[] = [];
  for (const m of text.matchAll(URL_RE)) {
    out.push(m[0].replace(/[.,;:!?]+$/, ""));
  }
  return out;
}

/** A compact display label for a URL: protocol dropped, trailing slash removed, capped in length. */
export function shortenUrl(url: string, max = 48): string {
  const bare = url.replace(/^https?:\/\//, "").replace(/\/+$/, "");
  return bare.length > max ? `${bare.slice(0, max - 1)}…` : bare;
}

/** The prominent PR link for the session note: the GitHub href (when the recorded value is a URL —
 * a bare number has no owner to build one), a short `#N` label, and the note's lifecycle status as
 * a proxy state chip. `null` when the note records no PR. */
export interface PrLink {
  href: string | null;
  label: string;
  status: string;
}

export function prLink(note: NoteDetail): PrLink | null {
  const pr = note.front.pr;
  if (pr === null || pr === undefined || pr === "") return null;
  const raw = String(pr);
  return { href: prHref(raw), label: prLabel(raw), status: note.front.status };
}

/** A resource reference surfaced from the note — a doc/issue/link the session pointed at. */
export interface Reference {
  url: string;
  label: string;
}

/** Every structured item across the note's sections, in the render order — used to harvest links. */
function allItems(front: NoteFront): NoteItem[] {
  return [
    ...front.done_when,
    ...front.blockers,
    ...front.todos,
    ...front.decisions,
    ...front.research,
    ...front.collab,
    ...front.ask,
  ];
}

/** Unique resource URLs across the note body and its items (and any item `link`), in first-seen
 * order, excluding the PR (rendered separately as its own button). */
export function collectReferences(note: NoteDetail): Reference[] {
  const seen = new Set<string>();
  const prUrl = note.front.pr ? prHref(String(note.front.pr)) : null;
  if (prUrl) seen.add(prUrl);

  const refs: Reference[] = [];
  const add = (url: string): void => {
    if (!seen.has(url)) {
      seen.add(url);
      refs.push({ url, label: shortenUrl(url) });
    }
  };

  for (const url of extractUrls(note.body)) add(url);
  for (const item of allItems(note.front)) {
    for (const url of extractUrls(item.text)) add(url);
    for (const url of extractUrls(item.link)) add(url);
  }
  return refs;
}

/** Fenced code blocks captured in the note body — the readable/code-formatted "session scripts"
 * (a verify/gate command block, a seed script, …). Each block's inner lines are joined; empty
 * blocks are dropped. Order-preserving. */
export function codeBlocks(body: string | null | undefined): string[] {
  if (!body) return [];
  const blocks: string[] = [];
  let current: string[] | null = null;
  for (const line of body.split("\n")) {
    if (/^\s*```/.test(line)) {
      if (current === null) {
        current = [];
      } else {
        const block = current.join("\n").trim();
        if (block) blocks.push(block);
        current = null;
      }
      continue;
    }
    if (current !== null) current.push(line);
  }
  return blocks;
}

/** One rendered note section: its display label, its items, and whether items are checkboxes
 * (todos / acceptance criteria) or plain bullets (decisions, research, …). */
export interface NoteSection {
  key: string;
  label: string;
  items: NoteItem[];
  checks: boolean;
}

/** The structured note sections in the canonical render order (`bn --cat`: done-when, blockers,
 * open todos, decisions, research, collab, asks), dropping any that are empty. Todos are filtered
 * to the still-open ones — a session view shows outstanding work, not the closed history. */
export function structuredSections(front: NoteFront): NoteSection[] {
  const openTodos = front.todos.filter((t) => !t.done);
  const sections: NoteSection[] = [
    { key: "done_when", label: "Done when", items: front.done_when, checks: true },
    { key: "blockers", label: "Blockers", items: front.blockers, checks: false },
    { key: "todos", label: "Open todos", items: openTodos, checks: true },
    { key: "decisions", label: "Decisions", items: front.decisions, checks: false },
    { key: "research", label: "Research", items: front.research, checks: false },
    { key: "collab", label: "Collab", items: front.collab, checks: false },
    { key: "ask", label: "Asks", items: front.ask, checks: false },
  ];
  return sections.filter((s) => s.items.length > 0);
}
