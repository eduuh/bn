#!/usr/bin/env node
/**
 * Compiles `design/cards/*.html` into standalone preview documents in `design/dist/`, ready to push
 * to a Claude Design project with the `DesignSync` tool.
 *
 * Cards are authored as **fragments** — a marker line plus body markup — and this script wraps each
 * one into a complete, self-contained HTML document. Self-contained is the hard requirement: the
 * Design System pane renders each card in isolation, so a card that only looks right when a sibling
 * stylesheet happens to be loaded is a card that lies about the system. Everything (tokens, the
 * compiled Tailwind, the page frame) is inlined.
 *
 * Tailwind is compiled **once** over the whole card set rather than per card. The class set here is
 * small and heavily shared, so one pass costs a fraction of 28 and — more importantly — guarantees
 * every card resolves a class identically.
 *
 * Usage: `npm run design:build`
 */
import { execFileSync } from "node:child_process";
import { mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const DESIGN = dirname(fileURLToPath(import.meta.url));
const CARDS = join(DESIGN, "cards");
const DIST = join(DESIGN, "dist");
const TMP = join(DESIGN, ".tmp");

/** `<!-- @dsCard group="Foundations" name="Colour ramp" width="880" height="620" -->` */
const DS_CARD = /^<!--\s*@dsCard\s+([^>]*?)-->/;
/** `<!-- @tokens clay -->` — which `design/tokens/*.css` block the card renders against. */
const TOKENS = /<!--\s*@tokens\s+([\w-]+)\s*-->/;

const attrs = (s) =>
  Object.fromEntries([...s.matchAll(/(\w+)="([^"]*)"/g)].map((m) => [m[1], m[2]]));

/** Compile the app's Tailwind over the card sources. Returns the stylesheet as a string. */
function compileTailwind() {
  mkdirSync(TMP, { recursive: true });
  const out = join(TMP, "ds.css");
  execFileSync(
    join(DESIGN, "..", "node_modules", ".bin", "tailwindcss"),
    [
      "-c",
      join(DESIGN, "tailwind.design.ts"),
      "-i",
      join(DESIGN, "input.css"),
      "-o",
      out,
      "--minify",
    ],
    { cwd: DESIGN, stdio: ["ignore", "ignore", "inherit"] },
  );
  return readFileSync(out, "utf8");
}

/**
 * The page frame every card sits in. Deliberately minimal: it establishes the app's background and
 * body type so a component is judged against the surface it actually lives on, and nothing else —
 * any padding or centring here would be design decisions the cards did not ask for.
 */
const frame = (tokens, css) => `
${tokens}
/* HEAD has no display face; keep the slot resolvable so \`font-display\` degrades to the body face
 * rather than to an empty family. Whether bn *should* have a display face is one of the questions
 * this bundle exists to answer. */
:root { --font-display: var(--font-display, var(--font-sans)); }
html { color-scheme: dark; }
body {
  margin: 0;
  padding: 24px;
  background: hsl(var(--background));
  color: hsl(var(--foreground));
  font-family: var(--font-sans);
  font-size: 14px;
  line-height: 1.5;
  -webkit-font-smoothing: antialiased;
}
${css}`;

function build() {
  rmSync(DIST, { recursive: true, force: true });
  mkdirSync(DIST, { recursive: true });

  const css = compileTailwind();
  const tokenSets = Object.fromEntries(
    readdirSync(join(DESIGN, "tokens"))
      .filter((f) => f.endsWith(".css"))
      .map((f) => [f.replace(/\.css$/, ""), readFileSync(join(DESIGN, "tokens", f), "utf8")]),
  );

  const files = readdirSync(CARDS)
    .filter((f) => f.endsWith(".html"))
    .sort();
  if (!files.length) throw new Error(`no cards found in ${CARDS}`);

  for (const file of files) {
    const src = readFileSync(join(CARDS, file), "utf8");

    const marker = src.match(DS_CARD);
    if (!marker) throw new Error(`${file}: missing a first-line <!-- @dsCard … --> marker`);
    const meta = attrs(marker[1]);
    if (!meta.group || !meta.name) throw new Error(`${file}: @dsCard needs both group and name`);

    const themeName = (src.match(TOKENS)?.[1] ?? "baseline").trim();
    const tokens = tokenSets[themeName];
    if (!tokens) throw new Error(`${file}: unknown token set "${themeName}"`);

    const body = src.replace(DS_CARD, "").replace(TOKENS, "").trim();

    // The @dsCard marker must be the document's first line — that is what the Design System pane
    // indexes on, so it sits ahead of the doctype rather than inside <head>.
    writeFileSync(
      join(DIST, file),
      `${marker[0]}
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${meta.name} — bn-web</title>
<style>${frame(tokens, css)}</style>
</head>
<body>
${body}
</body>
</html>
`,
    );
    console.log(
      `  ${meta.group.padEnd(12)} ${meta.name.padEnd(28)} ${themeName.padEnd(9)} ${file}`,
    );
  }

  rmSync(TMP, { recursive: true, force: true });
  console.log(`\n${files.length} cards → design/dist/`);
}

build();
