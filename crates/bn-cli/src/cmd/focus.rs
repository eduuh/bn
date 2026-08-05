//! `bn focus` — focus-session start/end/status.
use crate::*;

/// `bn focus start <dur>` — open a deep-work block. Rejects a malformed/zero duration. Human
/// view confirms the window and end time; `--json` returns the block id + end epoch.
pub(crate) fn emit_focus_start(dur: &str, as_json: bool) {
    use bn_core::clock::Clock;
    let now = bn_core::clock::RealClock.now_epoch_secs();
    let Some(secs) = bn_core::focus::parse_duration(dur) else {
        fail("bn focus start: duration must be like 90m, 2h, 1h30m, or a number of minutes");
    };
    let out =
        bn_core::focus::start(secs, now).unwrap_or_else(|e| fail(&format!("bn focus start: {e}")));
    if as_json {
        println!(
            "{}",
            Envelope::success("focus", payload::focus_start_data(&out)).to_json()
        );
    } else {
        println!(
            "\u{f05b} focus on — {} · ends {}",
            fmt_dur(secs),
            bn_core::util::clock_hm(out.ends_at)
        );
    }
}

/// `bn focus end` — close the active block early and drain the queue: the non-urgent actions the
/// block was holding back ("while you were heads-down: …"). Says so when no block was active.
pub(crate) fn emit_focus_end(cfg: &Config, as_json: bool) {
    use bn_core::clock::Clock;
    let clock = bn_core::clock::RealClock;
    let now = clock.now_epoch_secs();
    let ended = bn_core::focus::end(now).unwrap_or_else(|e| fail(&format!("bn focus end: {e}")));
    let queue = bn_core::next::drained(cfg, &clock.today(), now);
    // `bn q` is the sanctioned escape valve during a block, so whatever it banked is part of
    // the heads-down drain.
    let captures = bn_core::inbox::read(cfg);
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "focus",
                payload::focus_end_data(ended.as_ref(), &queue, &captures)
            )
            .to_json()
        );
        return;
    }
    match ended {
        None => println!("No focus block active"),
        Some(_) if queue.is_empty() && captures.is_empty() => {
            println!("\u{f05b} focus off — desk is clear ✓")
        }
        Some(_) => {
            println!(
                "\u{f05b} focus off — while you were heads-down: {}",
                drain_summary(&queue, captures.len())
            );
            for (i, a) in queue.iter().take(5).enumerate() {
                println!("{}", a.headline(i + 1));
            }
            for c in captures.iter().take(5) {
                println!("  ⚡ {} {}", c.id, c.text);
            }
            if !captures.is_empty() {
                println!("  (bn q drain to triage)");
            }
        }
    }
}

/// `bn focus status` (and bare `bn focus`) — the active block or that there's none. `--json`
/// exposes the focus state (`{ "active", "block": {...} | null }`) for the statusline.
pub(crate) fn emit_focus_status(as_json: bool) {
    use bn_core::clock::Clock;
    let now = bn_core::clock::RealClock.now_epoch_secs();
    let block = bn_core::focus::active(now);
    if as_json {
        println!(
            "{}",
            Envelope::success("focus", payload::focus_status_data(block.as_ref())).to_json()
        );
        return;
    }
    match block {
        Some(b) => println!("\u{f05b} focus · {}m left", b.remaining_mins()),
        None => println!("No focus block active"),
    }
}
