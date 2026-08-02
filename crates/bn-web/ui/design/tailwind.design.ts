import type { Config } from "tailwindcss";
import app from "../tailwind.config";

/**
 * Tailwind for the design-system preview bundle (`design/cards/*.html`).
 *
 * The app config is the single source of *structure* — the colour tokens, the radius ladder, the
 * font slots, the shadow scale. We reuse it wholesale so a card can never drift from what the real
 * components resolve to; the only thing overridden is `content`, which points at the cards instead
 * of `src/`.
 *
 * What is deliberately NOT inherited is the *palette*: every colour resolves through `hsl(var(--…))`
 * and the previews inject their own `:root` block (`design/tokens/*.css`) at build time. That split
 * is the whole point of the bundle — one set of components, swappable token sets, so a palette can
 * be judged against every component at once instead of one screen at a time.
 */
const config: Config = {
  ...app,
  content: ["./cards/**/*.html"],
};

export default config;
