//! The `bn tat` status header.
//!
//! These badges — attention and cron health — used to be `#()` segments in
//! tmux's `status-format[0]`. They were quiet on almost every 5s repaint, which is the worst thing
//! a status bar can be: it teaches you to stop reading it. They live here instead, on the surface
//! you already open to go somewhere (#1349). The bar kept only what changes with where you are.
//!
//! Two rules this module exists to keep:
//!
//! * **One source of truth per badge.** The producers already emit a tmux segment language
//!   (`#[fg=…]`), so this translates that to ANSI rather than growing a second set of colour rules.
//!   `bn_core::glyph`'s module doc lists what the mirrored glyph tables cost when they drift; a
//!   mirrored *colour* table would cost the same.
//! * **Nothing here runs on the 700 ms `subtick` repaint.** `preview.rs` sets the cost rules for
//!   that path. The header is painted on
//!   open and on `ctrl-r`, which is as fresh as a picker you hold open for seconds needs to be.
use bn_core::resolver::Config;
use bn_core::style::{Ink, Tone};

/// The project picker's key hints — the second header line, and the reason the header feed emits
/// two: `transform-header` replaces the whole header, not the line the status sits on.
pub(super) const HEADER_KEYS: &str =
    "Enter: open | ctrl-r: refresh | ctrl-x: kill | type repo/branch or paste a git URL";

/// The project picker's full header: the status line over the key hints. Always two lines, so the
/// list never shifts up and down as badges come and go.
///
/// The project picker only — these badges are machine-wide (this host's attention and cron),
/// so they belong on the list of everything, not inside one project. The
/// sub-picker also has no spare key to repaint on: its `ctrl-r` releases a pool slot.
pub(super) fn header(cfg: &Config) -> String {
    format!("{}\n{HEADER_KEYS}", status_line(cfg))
}

/// `bn tat --pick-stage statusline` — the `transform-header` feed.
pub(super) fn emit_status_line(cfg: &Config) {
    println!("{}", header(cfg));
}

/// The assembled header: attention · cron, each omitted when it has nothing to
/// say, and a dim `all quiet` when none of them do.
pub(super) fn status_line(cfg: &Config) -> String {
    let ink = Ink::always();
    let mut parts: Vec<String> = Vec::new();

    if let Some(attn) = crate::cmd::attn_badge::attn_badge() {
        parts.push(tmux_to_ansi(trim_trailing_divider(&attn), ink));
    }
    if let Some(cron) = crate::cmd::cron::machine_scope::running_badge(cfg) {
        parts.push(ink.tone(Tone::Accent, &cron));
    }

    if parts.is_empty() {
        return ink.tone(Tone::Meta, "all quiet");
    }
    parts.join(&format!(" {} ", ink.tone(Tone::Meta, "·")))
}

/// Drop a trailing `#[fg=…]· ` from a bar fragment, and the directive that only existed to colour
/// it. The attention badge ends with one because it was the last thing on `status-format[0]` and
/// that divider closed its run against the screen edge; this header draws its own separators, so
/// keeping it would print two in a row. Trimming at the consumer leaves `bn attn badge`'s output —
/// and the test pinning it — byte-for-byte what it was.
fn trim_trailing_divider(markup: &str) -> &str {
    let trimmed = markup.trim_end();
    let Some(head) = trimmed.strip_suffix('\u{b7}') else {
        return trimmed;
    };
    let head = head.trim_end();
    match head.rfind("#[") {
        Some(i) if head.ends_with(']') => head[..i].trim_end(),
        _ => head,
    }
}

// --- tmux markup -> ANSI ----------------------------------------------------

/// Translate a tmux status segment (`#[fg=…]text…`) into ANSI, so a producer written for the bar
/// can paint an fzf header unchanged.
///
/// Colours map onto [`Tone`] rather than onto truecolor escapes: the bar's hexes are hardcoded
/// tokyonight while the rest of bn paints through `bn_core::style`, and copying the hexes here
/// would make this the fourth palette in one workflow. `bg=`, `bold` and every other directive are
/// dropped — a header is one line of text, not a powerline.
fn tmux_to_ansi(markup: &str, ink: Ink) -> String {
    let mut out = String::new();
    let mut tone: Option<Tone> = None;
    let mut rest = markup;
    while let Some(open) = rest.find("#[") {
        emit_run(&mut out, &rest[..open], tone, ink);
        let after = &rest[open + 2..];
        let Some(close) = after.find(']') else {
            // An unterminated directive is not markup; take it literally rather than eating it.
            emit_run(&mut out, &rest[open..], tone, ink);
            return out;
        };
        tone = directive_tone(&after[..close]);
        rest = &after[close + 1..];
    }
    emit_run(&mut out, rest, tone, ink);
    out
}

/// Append `text`, painted in `tone` when there is one. Empty runs (two adjacent directives) add
/// nothing, so the output never carries a bare escape pair.
fn emit_run(out: &mut String, text: &str, tone: Option<Tone>, ink: Ink) {
    if text.is_empty() {
        return;
    }
    match tone {
        Some(t) => out.push_str(&ink.tone(t, text)),
        None => out.push_str(text),
    }
}

/// The tone a `#[…]` directive body selects, or `None` for "no tint from here on" (`default`, or a
/// directive that only sets a background).
fn directive_tone(body: &str) -> Option<Tone> {
    let mut tone = None;
    for field in body.split(',') {
        let field = field.trim();
        if field == "default" {
            tone = None;
        } else if let Some(colour) = field.strip_prefix("fg=") {
            tone = colour_tone(colour);
        }
    }
    tone
}

/// The bar's palette, named by meaning. The hexes are the tokyonight literals in `tmux.conf` and
/// `cmd/tmux_status.rs`; anything unrecognised stays untinted rather than guessing.
fn colour_tone(colour: &str) -> Option<Tone> {
    match colour {
        "red" | "#f7768e" => Some(Tone::Danger),
        "yellow" | "colour214" | "#e0af68" => Some(Tone::Warn),
        "green" | "#9ece6a" => Some(Tone::Ok),
        "#565f89" | "#3b4261" => Some(Tone::Meta),
        "cyan" | "#7aa2f7" | "#7dcfff" => Some(Tone::Accent),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: &str = "\u{1b}[31m";
    const RESET: &str = "\u{1b}[0m";

    #[test]
    fn plain_text_survives_untouched() {
        assert_eq!(tmux_to_ansi("all quiet", Ink::always()), "all quiet");
    }

    #[test]
    fn a_named_colour_paints_the_run_after_it() {
        assert_eq!(
            tmux_to_ansi("#[fg=red]12", Ink::always()),
            format!("{RED}12{RESET}")
        );
    }

    #[test]
    fn default_clears_the_tint() {
        assert_eq!(
            tmux_to_ansi("#[fg=red]12#[default] ok", Ink::always()),
            format!("{RED}12{RESET} ok")
        );
    }

    #[test]
    fn hex_and_colour_n_map_onto_the_palette() {
        let ink = Ink::always();
        assert_eq!(tmux_to_ansi("#[fg=#f7768e]x", ink), ink.danger("x"));
        assert_eq!(tmux_to_ansi("#[fg=colour214]x", ink), ink.warn("x"));
        assert_eq!(tmux_to_ansi("#[fg=#565f89]x", ink), ink.meta("x"));
    }

    #[test]
    fn a_background_only_directive_drops_the_tint_rather_than_painting() {
        assert_eq!(tmux_to_ansi("#[bg=#1f2335]x", Ink::always()), "x");
    }

    #[test]
    fn an_unterminated_directive_is_taken_literally() {
        assert_eq!(tmux_to_ansi("a#[fg=red", Ink::always()), "a#[fg=red");
    }

    #[test]
    fn adjacent_directives_emit_no_empty_escape_pair() {
        assert_eq!(tmux_to_ansi("#[fg=red]#[default]x", Ink::always()), "x");
    }

    #[test]
    fn ink_off_strips_every_escape() {
        assert_eq!(
            tmux_to_ansi("#[fg=red]12 #[fg=green]ok", Ink::never()),
            "12 ok"
        );
    }

    #[test]
    fn the_attention_fragments_trailing_divider_goes() {
        // exactly what `render_attn_badge` emits for "2 raised, nothing waiting"
        assert_eq!(
            trim_trailing_divider("#[fg=colour214]2\u{f0f3} #[fg=#3b4261]\u{b7} "),
            "#[fg=colour214]2\u{f0f3}"
        );
    }

    #[test]
    fn a_fragment_without_a_divider_is_only_right_trimmed() {
        assert_eq!(trim_trailing_divider("#[fg=red]12 "), "#[fg=red]12");
    }

    #[test]
    fn a_divider_not_preceded_by_a_directive_still_goes() {
        assert_eq!(trim_trailing_divider("12 \u{b7} "), "12");
    }
}
