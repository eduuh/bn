# bn-web design system bundle

Preview cards for the bn-web design system, built for a Claude Design project. The point of the
bundle is to see the whole system at once — every component against one palette — instead of judging
a colour by editing a component and reloading a single screen.

```sh
npm run design:build     # design/cards/*.html → design/dist/*.html
```

Then push `design/dist/` to the Claude Design project with the `DesignSync` tool.

## Layout

| path | |
|---|---|
| `cards/*.html` | card sources — a marker line plus body markup, authored as fragments |
| `tokens/*.css` | the palettes cards render against (`baseline`, `clay`) |
| `build.mjs` | compiles Tailwind over the cards and wraps each into a standalone document |
| `tailwind.design.ts` | the app's Tailwind config with `content` repointed at `cards/` |
| `input.css` | Tailwind entry + the border reset that preflight-off requires |
| `dist/` | build output — what gets pushed. Generated; git-ignored. |

## Authoring a card

```html
<!-- @dsCard group="Primitives" name="Button" width="880" height="560" -->
<!-- @tokens baseline -->
<div class="…">…</div>
```

- **`@dsCard` must be the first line.** The Design System pane builds its index from it, which is why
  explicit `register_assets` registration is not needed.
- **`@tokens`** picks the palette: any basename in `tokens/`. Defaults to `baseline`.
- Everything else is body markup. `build.mjs` supplies the document, the `<head>`, and the inlined
  stylesheet.

**Copy class strings from the real components — do not invent them.** A card that paraphrases
`components/ui/button.tsx` stops being evidence about the system and becomes a drawing of one. When
a class string here disagrees with the component, the component is right and the card is a bug.

## Groups

| group | what it answers |
|---|---|
| `Foundations` | colour ramp, signal scale, type, spacing/density, radius/elevation |
| `Primitives` | the `components/ui/` kit — button, card, badge, forms, overlays, states |
| `Components` | the composites carrying bn's identity — fleet card, stat tiles, terminal, markdown, both app shells, agent detail |
| `Candidates` | proposed directions, clearly separated from what ships today |

## Why the tokens are split out

Cards inherit *structure* from the app's own `tailwind.config.ts` — the radius ladder, the font
slots, the shadow scale, the semantic colour names. They do **not** inherit the palette: every colour
resolves through `hsl(var(--…))`, and `build.mjs` injects a `:root` block per card. One component
set, swappable palettes.

`tokens/baseline.css` is extracted from committed `HEAD` — what ships today. `tokens/clay.css` is a
proposed warm reskin, kept here so the `Candidates` cards can be judged against the same components
rather than described in prose. Adding a third direction is one file plus a `@tokens` line.

## Caveats

- **No fonts are bundled.** `--font-sans` and `--font-display` fall through to system faces, so the
  type cards show the fallback reality rather than the intended design. Fixing that is a deliberate
  step, not an oversight — the named faces are commercial and cannot be redistributed here.
- **`dist/` is generated.** Edit `cards/`, rebuild.
