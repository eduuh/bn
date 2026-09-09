//! `bn attn badge` — the attention lanes, as one status fragment.
//!
//! Its own module rather than a section of `cmd/attn.rs`, which sits on the 1000-line ratchet's
//! grandfathered list and may not gain a line (`.file-size-baseline`). Splitting it out is also
//! honest about what it became: no longer a status-bar-only renderer, but the one assembly of
//! "everything that wants you" shared by the `bn attn badge` command and `bn tat`'s header (#1349).
use crate::*;

/// Styled status-bar fragment for attention: ONE number — the total of everything that wants
/// you — with a bell glyph. Sums raised attention (cron failures + urgent/other items, via
/// `doctor::badge`) AND live agent windows (`waiting` needing input + `done`/ready, across all
/// sessions — shared with `bn tmux-attn-count` via [`tmux_status::attn_window_tally`]). Red when
/// anything is urgent (an urgent item, a cron failure, or a wait ignored 15+ minutes), amber
/// otherwise; nothing when all quiet, so the bar stays clean. The single attention producer in
/// status-format (the window tally used to be a separate `bn-attention-count.sh` fragment).
///
/// Returns the tmux-markup fragment (`#[fg=…]`), or `None` when everything is quiet. Split out of
/// [`emit_attn_badge`] so `bn tat`'s status header can reuse the same lanes without shelling out —
/// tat translates the markup to ANSI rather than growing a second set of colour rules (#1349).
pub(crate) fn attn_badge() -> Option<String> {
    let today = today();
    let b = bn_core::doctor::badge(&today);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let win = crate::proc::tmux_raw(&[
        "list-windows",
        "-a",
        "-F",
        "#{@bn_agent}\t#{@bn_done}\t#{@bn_agent_since}",
    ])
    .unwrap_or_default();
    let tally = crate::cmd::tmux_status::attn_window_tally(&win, now);
    crate::cmd::tmux_status::render_attn_badge(&b, &tally, notify_channel_is_dead(&today))
}

pub(crate) fn emit_attn_badge() {
    if let Some(badge) = attn_badge() {
        print!("{badge}");
    }
}

/// Whether today's notifications all failed to reach a backend — i.e. the push channel is broken
/// rather than deliberately quiet. The ledger is not rolled on load, so roll it first (as
/// `bn doctor` does) or a stale day's tallies leak into today's answer.
///
/// This is the difference between "bn chose not to interrupt you" and "bn tried and could not
/// reach you", which nothing on the bar could previously express.
fn notify_channel_is_dead(today: &str) -> bool {
    let mut ledger = bn_core::notify::policy::load_ledger();
    ledger.roll(today);
    ledger.sent_today == 0 && ledger.undeliverable() > 0
}
