/** Pure helpers for the live-pane view (#496). The terminal itself renders through xterm.js; these
 * cover the collapsed-summary preview, where we want a short, escape-free glimpse of the newest
 * output without standing up a terminal. */

// CSI / OSC / single-char (C1) escape sequences. tmux's `capture-pane -p` already strips most, but
// a stray sequence must never leak into the plain-text preview (that's the very bug #496 is fixing).
// Order matters: match the parameterised OSC/CSI forms before the bare single-char escapes, whose
// byte range (0x40–0x5F) otherwise overlaps the `[` / `]` introducers.
// eslint-disable-next-line no-control-regex
const ANSI_RE =
  /\u001b(?:\][^\u0007\u001b]*(?:\u0007|\u001b\\)|\[[0-?]*[ -/]*[@-~]|[@-Z\\^_])/g;

/** Strip ANSI/VT control sequences from a string, leaving printable text (box-drawing and other
 * Unicode glyphs are preserved). */
export function stripAnsi(s: string): string {
  return s.replace(ANSI_RE, "");
}

/** The newest non-empty pane line, escape-stripped and trimmed — the preview shown on the collapsed
 * live pane. Truncated so it stays a one-line glance. Empty string when there's nothing to show. */
export function lastPaneLine(lines: string[], max = 80): string {
  for (let i = lines.length - 1; i >= 0; i--) {
    const clean = stripAnsi(lines[i]).trim();
    if (clean) return clean.length > max ? clean.slice(0, max - 1) + "…" : clean;
  }
  return "";
}
