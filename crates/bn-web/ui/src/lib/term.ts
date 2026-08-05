/** Shared xterm.js setup for the live terminals (#491, reusing the #496 renderer). One home for the
 * palette + font so the polled tail and the streamed terminal read identically, and a `safeFit` that
 * never throws on a hidden/zero-size host. */
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

/** xterm palette mapped onto the dashboard's design tokens (see `index.css`) so agent ANSI colours
 * read on our dark surface instead of xterm's defaults. */
export const THEME = {
  background: "#0b0e13",
  foreground: "#d7dee7",
  cursor: "#4c8dff",
  cursorAccent: "#0b0e13",
  selectionBackground: "rgba(76, 141, 255, 0.30)",
  black: "#12161d",
  red: "#f85149",
  green: "#3fb950",
  yellow: "#d6a015",
  blue: "#4c8dff",
  magenta: "#a371f7",
  cyan: "#56d4dd",
  white: "#aab4c0",
  brightBlack: "#79838f",
  brightRed: "#ff7b72",
  brightGreen: "#56d364",
  brightYellow: "#e3b341",
  brightBlue: "#79b8ff",
  brightMagenta: "#c297ff",
  brightCyan: "#76e3ea",
  brightWhite: "#e7edf4",
} as const;

export const FONT_FAMILY =
  'ui-monospace, SFMono-Regular, "SF Mono", Menlo, "Cascadia Code", monospace';

/** Build a terminal + its fit addon. `interactive` flips `disableStdin` so the terminal can either
 * be a pure display (the streamed view) or accept the cursor (kept off by default — input is routed
 * through the dedicated composer so the mobile keyboard has a real field to focus). */
export function createTerminal(interactive = false): { term: Terminal; fit: FitAddon } {
  const term = new Terminal({
    convertEol: true,
    disableStdin: !interactive,
    cursorBlink: false,
    scrollback: 2000,
    fontFamily: FONT_FAMILY,
    fontSize: 12,
    lineHeight: 1.2,
    theme: THEME,
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  return { term, fit };
}

/** Fit the terminal to its host, but only when the host is actually laid out — fitting a hidden or
 * zero-height box throws or yields nonsense dimensions. Any fit failure is non-fatal (a later resize
 * or frame retries). */
export function safeFit(host: HTMLElement, fit: FitAddon): void {
  if (host.clientWidth <= 0 || host.clientHeight <= 0) return;
  try {
    fit.fit();
  } catch {
    /* transient layout race — the next tick reconciles */
  }
}
