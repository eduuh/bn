//! `bn staff mail <agent> [date]` — deliver a briefing card to Gmail as a reviewable draft.
//!
//! **This is an export, not a sink.** `staff.rs` states the sink set is closed and that nothing a
//! staff agent produces leaves the private notes — a doctrine worth keeping, since card text is
//! model-authored from untrusted packets. So delivery does not hook `route_card` at all: the
//! pipeline still routes only into attention/daily/note/dojo/research, and this command separately
//! *reads back* a card that has already been written and validated, then hands it to the mail
//! helper. Leaving the machine stays an explicit, separate act.
//!
//! Reading the feed store rather than the routing path also means this works for every agent,
//! including the bespoke members (`mentor`, `sentinel`, `curriculum`, `retro`) that write their
//! cards directly and never call `route_card`.

use bn_core::resolver::Config;
use std::io::Write;
use std::process::{Command, Stdio};

/// The helper that owns the Gmail conversation. Shipped beside the other job scripts.
const HELPER: &str = "mail-draft.py";

/// Deliver `agent`'s card for `date` (default: its most recent card) to Gmail as a draft.
///
/// Exit code is always 0 on a delivery failure. The card is already in `bn staff feed`; a missing
/// keychain entry or an unreachable IMAP server must not fail the morning's cron run, or a mail
/// problem would start masking the brief itself.
pub(crate) fn emit_staff_mail(cfg: &Config, agent: &str, date: Option<&str>) -> i32 {
    let Some((date, card)) = resolve_card(cfg, agent, date) else {
        eprintln!("bn staff mail: no card for '{agent}' — run `bn staff run {agent}` first");
        return 1;
    };

    let subject = format!("bn brief · {} · {}", card.date, card.headline);
    let body = bn_core::staff::render_card_md(&card);

    let Some(helper) = helper_path() else {
        eprintln!("bn staff mail: cannot find {HELPER} — leaving the card in the feed");
        return 0;
    };

    match pipe_to_helper(&helper, &subject, &body) {
        Ok(true) => {
            // A brief read in Gmail is a brief read. Without this the unread streak keeps climbing
            // and `staff_backpressure` pauses the agent after three days — the exact failure that
            // silenced this roster in the first place.
            //
            // Marking read is gated on the draft *actually landing*, never on the helper's exit
            // code: delivery failures are deliberately non-fatal, so a skipped draft exits 0 too.
            // Marking on exit code alone would consume the brief without a human ever seeing it.
            match bn_core::staff::mark_read(&date, agent) {
                Ok(true) => println!("bn staff mail: drafted and marked {agent} {date} read"),
                Ok(false) => println!("bn staff mail: drafted {agent} {date} (already read)"),
                Err(e) => eprintln!("bn staff mail: drafted, but could not mark read: {e}"),
            }
            0
        }
        // No draft landed — the helper said why on stderr. Leave the card unread so the brief is
        // still waiting in `bn staff feed`.
        Ok(false) => 0,
        Err(e) => {
            eprintln!("bn staff mail: could not run the mail helper ({e})");
            0
        }
    }
}

/// The card to send: `date`'s if given, else the agent's most recent.
fn resolve_card(
    cfg: &Config,
    agent: &str,
    date: Option<&str>,
) -> Option<(String, bn_core::staff::BriefingCard)> {
    if let Some(d) = date {
        let card = bn_core::staff::read_card(&bn_core::staff::card_path(cfg, d, agent))?;
        return Some((d.to_string(), card));
    }
    let mut refs: Vec<_> = bn_core::staff::list_cards(cfg)
        .into_iter()
        .filter(|c| c.agent == agent)
        .collect();
    refs.sort_by(|a, b| b.date.cmp(&a.date));
    let newest = refs.into_iter().next()?;
    let card = bn_core::staff::read_card(&newest.path)?;
    Some((newest.date, card))
}

/// Locate the mail helper: the installed jobs dir first (where cron finds it), then this
/// checkout's `config/jobs/` so a working tree can be tested before it is installed.
fn helper_path() -> Option<std::path::PathBuf> {
    let installed = bn_core::resolver::jobs_dir().join(HELPER);
    if installed.is_file() {
        return Some(installed);
    }
    let local = std::env::current_dir().ok()?.join("config/jobs").join(HELPER);
    local.is_file().then_some(local)
}

/// The helper's stdout token meaning a draft really landed in the mailbox.
const DRAFTED: &str = "drafted";

/// Feed `subject\nbody` to the helper on stdin. `Ok(true)` only when it reported [`DRAFTED`] —
/// its exit status is always 0, so it cannot be used to tell delivery from a clean skip.
fn pipe_to_helper(helper: &std::path::Path, subject: &str, body: &str) -> std::io::Result<bool> {
    let mut child = Command::new("python3")
        .arg(helper)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        // Subject on line one, body after — the contract the helper parses.
        write!(stdin, "{subject}\n{body}")?;
    }
    let out = child.wait_with_output()?;
    Ok(out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == DRAFTED)
}
