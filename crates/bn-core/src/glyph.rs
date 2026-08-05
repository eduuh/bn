//! The terminal icon vocabulary — Nerd Font glyphs, single-width, tinted by ANSI color.
//!
//! Colored pictographic emoji (🔔 ✅ 📅 …) render as double-width bitmap graphics that ignore
//! the terminal theme and misalign columns in fzf/tmux; these Private-Use-Area codepoints are
//! drawn by the font instead (Font Awesome 4.7 range + one Octicon, shipped by every Nerd Font
//! and by Ghostty's built-in fallback), so they behave like any other character.
//!
//! Surface rule: these are for TERMINAL OUTPUT ONLY. PUA codepoints are tofu on GitHub and in
//! unpatched fonts, so anything persisted (note markdown, brag docs, synced files) must keep
//! plain Unicode (✓ ★ ⚠ •). Mirrors of this set live in `config/workflow/tmux.conf`
//! (window-status-format), `config/workflow/bin/tmux-agent.sh`, and the native tmux status segments in
//! `crates/bn-cli/src/cmd/tmux_status.rs` (`bn tmux-git`/`tmux-goal`), plus
//! `nvim/lua/bn/util.lua` — keep them in sync when a
//! glyph changes (`bn glyphs` renders the vocabulary for eyeballing).

/// nf-fa-bell — an agent or item is waiting on you.
pub const BELL: &str = "\u{f0f3}";
/// nf-fa-check — done.
pub const CHECK: &str = "\u{f00c}";
/// nf-fa-ban — blocked / blocker / review holding you.
pub const BAN: &str = "\u{f05e}";
/// nf-fa-cog — working / auto mode.
pub const GEAR: &str = "\u{f013}";
/// nf-fa-eye — WAITING ON YOU (others blocked on you).
pub const EYE: &str = "\u{f06e}";
/// nf-fa-calendar — TODAY / meetings.
pub const CALENDAR: &str = "\u{f073}";
/// nf-fa-bolt — BREAKING (on fire right now).
pub const BOLT: &str = "\u{f0e7}";
/// nf-fa-tasks — YOUR WORK (your own backlog).
pub const TASKS: &str = "\u{f0ae}";
/// nf-fa-envelope — email.
pub const ENVELOPE: &str = "\u{f0e0}";
/// nf-fa-comment — a Teams/chat mention.
pub const COMMENT: &str = "\u{f075}";
/// nf-oct-git-pull-request — a PR.
pub const PULL_REQUEST: &str = "\u{f407}";
/// nf-fa-crosshairs — focus block.
pub const TARGET: &str = "\u{f05b}";
/// nf-fa-sign-out — away (afk) mode.
pub const AWAY: &str = "\u{f08b}";
/// nf-fa-pencil — open todos count.
pub const PENCIL: &str = "\u{f040}";
/// nf-fa-clock-o — SLA / wait-time / cron schedule.
pub const CLOCK: &str = "\u{f017}";

#[cfg(test)]
mod tests {
    #[test]
    fn glyphs_are_single_scalar_private_use_no_vs16() {
        for g in [
            super::BELL,
            super::CHECK,
            super::BAN,
            super::GEAR,
            super::EYE,
            super::CALENDAR,
            super::BOLT,
            super::TASKS,
            super::ENVELOPE,
            super::COMMENT,
            super::PULL_REQUEST,
            super::TARGET,
            super::AWAY,
            super::PENCIL,
            super::CLOCK,
        ] {
            let chars: Vec<char> = g.chars().collect();
            // One scalar, no U+FE0F emoji-presentation selector — the selector is what forces
            // the colored bitmap rendering this module exists to avoid.
            assert_eq!(chars.len(), 1, "{g:?} must be a single codepoint");
            let c = chars[0] as u32;
            assert!((0xE000..=0xF8FF).contains(&c), "{g:?} must be PUA");
        }
    }
}
