import hljs from "highlight.js/lib/core";
import bash from "highlight.js/lib/languages/bash";
import css from "highlight.js/lib/languages/css";
import diff from "highlight.js/lib/languages/diff";
import go from "highlight.js/lib/languages/go";
import ini from "highlight.js/lib/languages/ini";
import javascript from "highlight.js/lib/languages/javascript";
import json from "highlight.js/lib/languages/json";
import markdown from "highlight.js/lib/languages/markdown";
import python from "highlight.js/lib/languages/python";
import rust from "highlight.js/lib/languages/rust";
import sql from "highlight.js/lib/languages/sql";
import typescript from "highlight.js/lib/languages/typescript";
import xml from "highlight.js/lib/languages/xml";
import yaml from "highlight.js/lib/languages/yaml";

/**
 * Syntax highlighting for fenced code blocks, imported only from `MarkdownBody` so it rides the
 * markdown chunk that is already lazy — the entry bundle is unaffected. Measured cost on that
 * chunk: 48 kB → 72 kB gzipped.
 *
 * Languages are registered explicitly rather than via `highlight.js`'s all-languages entry point,
 * which pulls ~190 grammars. The set below is deliberately not trimmed further: dropping the four
 * least-likely (css/go/sql/xml) saves only ~7 kB gzipped, and bn is a *cross-repo* dashboard — a
 * note here can quote code from any repo the user works in, and an unhighlighted block is a worse
 * outcome than a few kB on a chunk that only loads when markdown is actually on screen.
 */
type LanguageFn = Parameters<typeof hljs.registerLanguage>[1];

const LANGUAGES: Record<string, LanguageFn> = {
  bash,
  css,
  diff,
  go,
  ini,
  javascript,
  json,
  markdown,
  python,
  rust,
  sql,
  typescript,
  xml,
  yaml,
};

for (const [name, fn] of Object.entries(LANGUAGES)) hljs.registerLanguage(name, fn);

/**
 * Info-string spellings that agents and humans actually write, mapped onto a registered grammar.
 * `highlight.js` already knows many of these as built-in aliases; the ones here are the spellings
 * it would otherwise miss (or that we deliberately redirect, like `toml` → the `ini` grammar).
 */
const ALIASES: Record<string, string> = {
  sh: "bash",
  shell: "bash",
  zsh: "bash",
  console: "bash",
  "shell-session": "bash",
  ts: "typescript",
  tsx: "typescript",
  js: "javascript",
  jsx: "javascript",
  py: "python",
  rs: "rust",
  yml: "yaml",
  md: "markdown",
  patch: "diff",
  toml: "ini",
  html: "xml",
  svg: "xml",
};

/** The `language-x` class react-markdown puts on a fenced block's `<code>`, as a bare name. */
export function languageOf(className: string | undefined): string | undefined {
  const match = /\blanguage-([\w+-]+)/.exec(className ?? "");
  return match?.[1]?.toLowerCase();
}

/** The registered grammar for an info string, or `undefined` if we don't highlight it. */
export function grammarFor(language: string | undefined): string | undefined {
  if (!language) return undefined;
  const name = ALIASES[language] ?? language;
  return hljs.getLanguage(name) ? name : undefined;
}

/**
 * Highlighted HTML for `code`, or `null` when the block has no info string or names a grammar we
 * don't ship — in which case the caller renders the plain text instead of guessing. Auto-detection
 * is deliberately not used: on a two-line block it guesses wrong more often than it helps.
 *
 * The returned string is safe to inject: `highlight.js` HTML-escapes the source it is given, so a
 * note body can't smuggle markup through a code fence (`ignoreIllegals` keeps a malformed snippet
 * from throwing instead of degrading).
 */
export function highlightCode(code: string, language: string | undefined): string | null {
  const grammar = grammarFor(language);
  if (!grammar) return null;
  try {
    return hljs.highlight(code, { language: grammar, ignoreIllegals: true }).value;
  } catch {
    return null;
  }
}
