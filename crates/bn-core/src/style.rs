//! The terminal colour vocabulary — one semantic palette, gated on whether anyone can see it.
//!
//! Colour in bn's output means exactly one thing: **severity**. Red is on fire, yellow wants a
//! look, green is clear, dim is metadata you read only after the line has earned your attention.
//! Nothing else is tinted, so a red row never has to compete with a red decoration. The structural
//! layer — section headers, the glyph vocabulary in [`crate::glyph`], the ` · ` metadata separator
//! — carries the rest of the hierarchy without spending colour on it.
//!
//! # The two surfaces
//!
//! bn writes coloured text to two destinations, and they do **not** share a colour rule:
//!
//! | surface | destination | painter |
//! | ------- | ----------- | ------- |
//! | stdout  | the user's terminal, or a pipe/file | [`Ink::auto`] — obeys [`enabled`] |
//! | fzf     | `fzf`'s stdin, rendered by `fzf --ansi` | [`Ink::always`] — never gated |
//!
//! The distinction matters because [`enabled`] asks whether *bn's own stdout* is a terminal, and
//! the strings bn feeds to `fzf` never are — they go down a pipe that `fzf` then renders on bn's
//! behalf. Gating those on [`enabled`] would strip exactly the escapes `--ansi` was passed to
//! interpret, so the fzf row builders take an explicit [`Ink::always`].
//!
//! The free functions ([`danger`], [`meta`], …) are the stdout surface — they are
//! [`Ink::auto`] shims, so a plain `style::danger(…)` is always the safe default. Reach for
//! [`Ink::always`] only where the string is about to be handed to `fzf --ansi`, and pair it with
//! [`strip_unless_enabled`] on the fzf-is-missing fallback path, which prints those same rows
//! straight to stdout.
//!
//! ```
//! use bn_core::style;
//! let row = format!("{} {}", style::danger("⚠ cron main-sync"), style::meta("39 failures"));
//!
//! // A row destined for `fzf --ansi` instead:
//! let ink = style::Ink::always();
//! let picked = format!("{} {}", ink.danger("⚠ cron main-sync"), ink.meta("39 failures"));
//! ```
//!
//! ## When colour is off
//!
//! Resolved once per process by [`enabled`], in precedence order:
//!
//! | signal                       | result                                        |
//! | ---------------------------- | --------------------------------------------- |
//! | `BN_COLOR=always\|never\|auto` | explicit override, checked first              |
//! | `NO_COLOR` set (any value)   | off — <https://no-color.org>                  |
//! | `CLICOLOR_FORCE` set non-`0` | on, even into a pipe                          |
//! | `TERM=dumb`                  | off                                           |
//! | stdout is not a TTY          | off — the pipe/redirect case                  |
//!
//! That last row is the one that matters day to day: before this module existed bn wrote escapes
//! unconditionally, so `bn attn > today.txt` and `bn doctor | grep urgent` both embedded raw
//! `\x1b[2m` in their output. Anything that renders for a *file* rather than a terminal — the JSON
//! envelope, note markdown, brag docs — must not call into here at all; see the surface rule in
//! [`crate::glyph`].

use std::sync::OnceLock;

/// SGR reset. Every helper closes with this rather than resetting only the attribute it set, so a
/// truncated line can never leak styling into the next one.
///
/// Public so a caller building its own `{OPEN}text{RESET}` constants can derive them from this
/// palette instead of respelling the codes — see [`Tone::sgr`].
pub const RESET: &str = "\x1b[0m";

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";

static ENABLED: OnceLock<bool> = OnceLock::new();
static ENABLED_ERR: OnceLock<bool> = OnceLock::new();

/// Whether colour is on for this process, resolved once and cached.
///
/// Deliberately reads the environment only on the first call: bn's renderers ask this per *row*,
/// and a process that starts writing plain text should not start writing escapes halfway down a
/// list because something else in the process mutated the environment.
pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| resolve(Stream::Out))
}

/// The same question for stderr, answered separately.
///
/// The two streams get redirected independently — `bn wt merge 2>build.log` leaves a terminal on
/// stdout and a file on stderr — so a single answer is wrong for one of them whichever way it
/// goes. Diagnostics and error text are the stderr surface; everything else is [`enabled`].
pub fn enabled_stderr() -> bool {
    *ENABLED_ERR.get_or_init(|| resolve(Stream::Err))
}

/// Which stream a colour decision is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stream {
    Out,
    Err,
}

/// The decision table from the module docs. Split out so the tests can exercise it directly
/// without the `OnceLock` freezing the first answer for the whole test binary.
fn resolve(stream: Stream) -> bool {
    resolve_from(
        std::env::var("BN_COLOR").ok().as_deref(),
        std::env::var_os("NO_COLOR").is_some(),
        std::env::var("CLICOLOR_FORCE").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
        {
            use std::io::IsTerminal;
            match stream {
                Stream::Out => std::io::stdout().is_terminal(),
                Stream::Err => std::io::stderr().is_terminal(),
            }
        },
    )
}

/// Pure core of [`resolve`] — every signal passed in, so the precedence order is unit-testable.
fn resolve_from(
    bn_color: Option<&str>,
    no_color: bool,
    clicolor_force: Option<&str>,
    term: Option<&str>,
    is_tty: bool,
) -> bool {
    // An explicit BN_COLOR wins over everything, including NO_COLOR: it is the knob a user reaches
    // for precisely when the ambient environment is already wrong.
    match bn_color.map(str::trim) {
        Some("always") | Some("force") => return true,
        Some("never") | Some("none") => return false,
        _ => {}
    }
    if no_color {
        return false;
    }
    if clicolor_force.is_some_and(|v| !v.trim().is_empty() && v.trim() != "0") {
        return true;
    }
    if term == Some("dumb") {
        return false;
    }
    is_tty
}

/// One entry in the palette, named for what it *means* rather than what colour it is.
///
/// Exists for the call sites that have to decide on a tint somewhere other than where they format
/// the string — a `match` over a status enum that feeds a later `format!`, say. Those used to
/// carry a bare escape-code fragment (`"1;31"`) across the gap; they carry a `Tone` now, so the
/// escape never exists outside this module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// On fire — a failing cron, a blocker, an urgent attention item, a dead agent.
    Danger,
    /// Wants a look but is not burning — a warn-severity item, a stale branch.
    Warn,
    /// Clear — nothing on the list, a passing gate, work that finished.
    Ok,
    /// Ids, counts, paths, timestamps, repo/branch locators.
    Meta,
    /// Structure. Bold and uncoloured on purpose.
    Head,
    /// A link or id the reader is expected to act on. Reads as "interactive", not "wrong".
    Accent,
}

impl Tone {
    /// The SGR escape this tone opens with.
    ///
    /// `const`, so a module that has to name colours in `const` items — `wt`'s `{RED}…{NC}` idiom,
    /// which predates this palette — can derive them here rather than keeping a second copy of the
    /// codes that drifts. Pair with [`RESET`]. Prefer [`Ink`] anywhere a function call will do.
    pub const fn sgr(self) -> &'static str {
        match self {
            Tone::Danger => RED,
            Tone::Warn => YELLOW,
            Tone::Ok => GREEN,
            Tone::Meta => DIM,
            Tone::Head => BOLD,
            Tone::Accent => CYAN,
        }
    }

    /// The severity ramp shared by `bn attn`, `bn doctor` and the attention rollups: `urgent` is
    /// [`Tone::Danger`], `warn` is [`Tone::Warn`], and anything else (`info`, and any severity a
    /// future writer invents) stays [`Tone::Meta`] rather than inventing a colour for itself.
    pub fn from_severity(sev: &str) -> Self {
        match sev {
            "urgent" => Tone::Danger,
            "warn" => Tone::Warn,
            _ => Tone::Meta,
        }
    }
}

/// A painter carrying an explicit "is colour on" bit.
///
/// The bit is a constructor choice rather than an ambient global so that every call site names the
/// surface it writes to — see the table in the module docs. Copy and one byte wide; build one per
/// row-rendering function and pass it around by value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ink(bool);

impl Ink {
    /// The stdout surface: paints only when [`enabled`] says someone can see it.
    pub fn auto() -> Self {
        Ink(enabled())
    }

    /// The `fzf --ansi` surface: always paints, because the pipe bn writes into is not the
    /// terminal that ends up rendering the row.
    pub const fn always() -> Self {
        Ink(true)
    }

    /// Never paints. For tests, and for callers that have already decided the destination is a
    /// file.
    pub const fn never() -> Self {
        Ink(false)
    }

    /// Whether this painter is actually painting.
    pub const fn is_on(self) -> bool {
        self.0
    }

    /// Paint `text` in `tone`, or hand it back untouched when this painter is off.
    pub fn tone(self, tone: Tone, text: &str) -> String {
        if self.0 {
            format!("{}{text}{RESET}", tone.sgr())
        } else {
            text.to_string()
        }
    }

    /// See [`Tone::Danger`].
    pub fn danger(self, text: &str) -> String {
        self.tone(Tone::Danger, text)
    }

    /// See [`Tone::Warn`].
    pub fn warn(self, text: &str) -> String {
        self.tone(Tone::Warn, text)
    }

    /// See [`Tone::Ok`].
    pub fn ok(self, text: &str) -> String {
        self.tone(Tone::Ok, text)
    }

    /// See [`Tone::Meta`].
    pub fn meta(self, text: &str) -> String {
        self.tone(Tone::Meta, text)
    }

    /// See [`Tone::Head`].
    pub fn head(self, text: &str) -> String {
        self.tone(Tone::Head, text)
    }

    /// See [`Tone::Accent`].
    pub fn accent(self, text: &str) -> String {
        self.tone(Tone::Accent, text)
    }

    /// See [`Tone::from_severity`].
    pub fn severity(self, sev: &str, text: &str) -> String {
        self.tone(Tone::from_severity(sev), text)
    }

    /// The opening escape for `tone`, or `""` when this painter is off.
    ///
    /// The escape hatch for a block that interleaves many short runs inside one format string,
    /// where a helper call per fragment reads worse than an explicit pair — `bn tmux-help`'s
    /// cheat-sheet is the case that earned it. The codes still live here, which is the part that
    /// has to stay in one place. Always pair with [`Ink::close`].
    pub fn open(self, tone: Tone) -> &'static str {
        if self.0 {
            tone.sgr()
        } else {
            ""
        }
    }

    /// The reset that closes [`Ink::open`], or `""` when this painter is off.
    pub fn close(self) -> &'static str {
        if self.0 {
            RESET
        } else {
            ""
        }
    }
}

/// On fire — a failing cron, a blocker, an urgent attention item, a dead agent.
pub fn danger(text: &str) -> String {
    Ink::auto().danger(text)
}

/// Wants a look but is not burning — a warn-severity item, a stale branch, a non-zero blocker
/// count sitting inside an otherwise ordinary row.
pub fn warn(text: &str) -> String {
    Ink::auto().warn(text)
}

/// Clear — nothing on the list, a passing gate, work that finished.
pub fn ok(text: &str) -> String {
    Ink::auto().ok(text)
}

/// Metadata: ids, counts, paths, timestamps, repo/branch locators. Present for when you want it,
/// out of the way until then.
pub fn meta(text: &str) -> String {
    Ink::auto().meta(text)
}

/// A section header. Bold and uncoloured on purpose — sections are structure, and tinting them
/// would put a second red on screen competing with the rows that are actually on fire.
pub fn head(text: &str) -> String {
    Ink::auto().head(text)
}

/// A link or an id the reader is expected to act on (open, copy, pass to another command). The one
/// non-severity use of colour, kept to cyan so it reads as "interactive", not "wrong".
pub fn accent(text: &str) -> String {
    Ink::auto().accent(text)
}

/// Paint `text` in `tone` on the stdout surface. For call sites that pick a tone somewhere other
/// than where they format the string — see [`Tone`].
pub fn tone(tone: Tone, text: &str) -> String {
    Ink::auto().tone(tone, text)
}

/// The severity ramp on the stdout surface. See [`Tone::from_severity`].
pub fn severity(sev: &str, text: &str) -> String {
    Ink::auto().severity(sev, text)
}

/// Remove every SGR (colour/attribute) escape from `text`, leaving other escapes alone.
///
/// Non-SGR sequences survive on purpose: bn also emits terminal *control* — `\x1b[2J\x1b[H` to
/// repaint the orchestrate dashboard, the termios dance in the picker — and stripping those would
/// break the screen rather than clean it.
pub fn strip(text: &str) -> String {
    if !text.contains('\x1b') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('\x1b') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        match sgr_len(tail) {
            Some(n) => rest = &tail[n..],
            None => {
                // Not styling — keep the ESC and carry on past it.
                out.push('\x1b');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Byte length of the complete SGR sequence at the start of `s` (`ESC [ digits/semicolons m`), or
/// `None` when `s` does not begin with one.
fn sgr_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.len() < 3 || b[0] != 0x1b || b[1] != b'[' {
        return None;
    }
    let mut i = 2;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b';') {
        i += 1;
    }
    (i < b.len() && b[i] == b'm').then_some(i + 1)
}

/// Hand back a row built for the `fzf --ansi` surface, flattened when this process is not painting.
///
/// The fzf row builders paint with [`Ink::always`], which is right while `fzf` is doing the
/// rendering — but every one of those call sites also has a fallback that prints the same rows
/// straight to stdout when `fzf` is missing from `PATH`. On that path the row is ordinary stdout
/// again and has to obey [`enabled`], or `bn jump > out.txt` on a box without `fzf` writes raw
/// escapes into the file.
pub fn strip_unless_enabled(text: &str) -> String {
    if enabled() {
        text.to_string()
    } else {
        strip(text)
    }
}

/// [`strip_unless_enabled`] for the stderr surface. See [`enabled_stderr`].
pub fn strip_unless_enabled_err(text: &str) -> String {
    if enabled_stderr() {
        text.to_string()
    } else {
        strip(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_TONES: [Tone; 6] = [
        Tone::Danger,
        Tone::Warn,
        Tone::Ok,
        Tone::Meta,
        Tone::Head,
        Tone::Accent,
    ];

    #[test]
    fn tty_decides_when_nothing_is_set() {
        assert!(resolve_from(
            None,
            false,
            None,
            Some("xterm-256color"),
            true
        ));
        assert!(!resolve_from(
            None,
            false,
            None,
            Some("xterm-256color"),
            false
        ));
    }

    #[test]
    fn no_color_beats_a_tty() {
        assert!(!resolve_from(
            None,
            true,
            None,
            Some("xterm-256color"),
            true
        ));
    }

    #[test]
    fn bn_color_beats_no_color_in_both_directions() {
        assert!(resolve_from(Some("always"), true, None, None, false));
        assert!(!resolve_from(Some("never"), false, None, None, true));
    }

    #[test]
    fn clicolor_force_paints_into_a_pipe_but_zero_does_not() {
        assert!(resolve_from(None, false, Some("1"), None, false));
        assert!(!resolve_from(None, false, Some("0"), None, false));
        assert!(!resolve_from(None, false, Some(""), None, false));
    }

    #[test]
    fn dumb_terminals_stay_plain() {
        assert!(!resolve_from(None, false, None, Some("dumb"), true));
    }

    #[test]
    fn unknown_severities_fall_back_to_metadata_not_a_new_colour() {
        // `from_severity` is the only mapping that dispatches on caller data, so it is the only
        // one that can be handed a value nobody planned for.
        for sev in ["info", "", "notice", "critical"] {
            assert_eq!(
                Tone::from_severity(sev),
                Tone::Meta,
                "severity({sev:?}) invented a colour"
            );
        }
        assert_eq!(Tone::from_severity("urgent"), Tone::Danger);
        assert_eq!(Tone::from_severity("warn"), Tone::Warn);
    }

    #[test]
    fn the_fzf_surface_paints_whatever_stdout_is_doing() {
        // The whole reason `Ink` exists: these rows go down a pipe to `fzf --ansi`, so they must
        // carry escapes even in the test binary, where stdout is captured and `enabled()` is off.
        let ink = Ink::always();
        for tone in ALL_TONES {
            let painted = ink.tone(tone, "x");
            assert!(painted.starts_with(tone.sgr()), "{painted:?}");
            assert!(painted.ends_with(RESET), "{painted:?} leaks styling");
        }
    }

    #[test]
    fn a_painter_that_is_off_is_pure_identity() {
        // The contract call sites rely on: with colour off the helpers must hand back exactly what
        // they were given, so width/truncation maths never has to know about escapes.
        let ink = Ink::never();
        assert!(!ink.is_on());
        for tone in ALL_TONES {
            assert_eq!(ink.tone(tone, "x"), "x");
        }
        assert_eq!(ink.severity("urgent", "x"), "x");
    }

    #[test]
    fn the_free_functions_track_the_auto_painter() {
        let ink = Ink::auto();
        assert_eq!(danger("x"), ink.danger("x"));
        assert_eq!(warn("x"), ink.warn("x"));
        assert_eq!(ok("x"), ink.ok("x"));
        assert_eq!(meta("x"), ink.meta("x"));
        assert_eq!(head("x"), ink.head("x"));
        assert_eq!(accent("x"), ink.accent("x"));
        assert_eq!(severity("urgent", "x"), ink.danger("x"));
    }

    #[test]
    fn strip_undoes_every_tone() {
        let ink = Ink::always();
        for tone in ALL_TONES {
            assert_eq!(strip(&ink.tone(tone, "hello")), "hello");
        }
        // Nested and adjacent runs, which is what a real composed row looks like.
        let row = format!("{}  {}", ink.danger("⚠ cron"), ink.meta("39 failures"));
        assert_eq!(strip(&row), "⚠ cron  39 failures");
    }

    #[test]
    fn strip_leaves_plain_text_and_cursor_control_alone() {
        assert_eq!(strip("no escapes here"), "no escapes here");
        assert_eq!(strip(""), "");
        // Clear-screen + cursor-home is control, not styling: `bn orchestrate`'s dashboard repaint
        // depends on it surviving.
        assert_eq!(strip("\x1b[2J\x1b[Hframe"), "\x1b[2J\x1b[Hframe");
        // A lone ESC, and a truncated sequence, must not eat the rest of the line.
        assert_eq!(strip("a\x1bb"), "a\x1bb");
        assert_eq!(strip("a\x1b[31"), "a\x1b[31");
    }

    #[test]
    fn strip_is_byte_exact_on_multibyte_rows() {
        // Glyphs are PUA codepoints and titles carry arbitrary Unicode, so `strip` slices on char
        // boundaries or it panics in production.
        let ink = Ink::always();
        let row = ink.warn("\u{f0f3} needs you — 東京 🎉");
        assert_eq!(strip(&row), "\u{f0f3} needs you — 東京 🎉");
    }

    #[test]
    fn strip_unless_enabled_matches_the_process_decision() {
        let ink = Ink::always();
        let row = ink.danger("⚠ cron");
        if enabled() {
            assert_eq!(strip_unless_enabled(&row), row);
        } else {
            assert_eq!(strip_unless_enabled(&row), "⚠ cron");
        }
    }
}
