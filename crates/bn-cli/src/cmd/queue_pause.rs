//! `bn queue pause` / `resume` — the drain; and `bn queue off` / `on` — the switch.
//!
//! Its own module because `queue.rs` is at the 1000-line limit, and because these four are one
//! concern: what it means to stop starting things. The policy they write into the event log is read
//! back by [`bn_core::queue::pause::PauseState`] and [`bn_core::queue::off`] — nothing here decides
//! anything, it only records the ask.
//!
//! The pair worth keeping straight, because they sound alike and do opposite things to the backlog:
//! **pause holds work** (it keeps accumulating and starts when the pause lifts), **off stops work
//! being filed** (every trigger runs on the spot, and there is nothing to catch up on). Each
//! command's own output says which it is, for exactly that reason.
use crate::*;

/// `bn queue pause --scope <s> [--reason] [--for]` — hold new work on a scope.
///
/// An unparseable scope is an error, never a fallback: silently widening a `repo:typo` into `all`
/// would stop the whole machine on behalf of someone who asked for one repo, and silently narrowing
/// it to nothing would report a drain that never happened.
pub(crate) fn emit_pause(scope: &str, reason: &str, for_: Option<&str>, now: i64, json: bool) {
    let Some(parsed) = bn_core::queue::pause::PauseScope::parse(scope) else {
        fail_json(
            json,
            "queue_pause",
            "bad-scope",
            &format!(
                "bn queue pause: unknown scope '{scope}' (use: all, kind:<k>, lane:coding|research, node:<id>, repo:<name>)"
            ),
        );
    };
    let until = match for_ {
        Some(dur) => match bn_core::focus::parse_duration(dur).filter(|s| *s > 0) {
            Some(secs) => Some(now + secs),
            None => fail_json(
                json,
                "queue_pause",
                "bad-duration",
                &format!("bn queue pause: cannot read --for '{dur}' (try 90m, 2h)"),
            ),
        },
        None => None,
    };
    if let Err(e) = bn_core::queue::pause::pause(&parsed, reason, until, now) {
        fail_json(
            json,
            "queue_pause",
            "write-failed",
            &format!("bn queue pause: {e}"),
        );
    }
    let key = parsed.key();
    if json {
        println!(
            "{}",
            Envelope::success(
                "queue_pause",
                serde_json::json!({ "scope": key, "reason": reason, "until": until }),
            )
            .to_json()
        );
        return;
    }
    // Say what a pause does *not* do. "Paused" reads as "stopped" to everyone who has not read the
    // module doc, and the difference is the entire feature.
    println!("bn queue pause: {key} held — work already running finishes; nothing new is admitted");
    if let Some(until) = until {
        println!("bn queue pause: auto-resumes at {}", bn_core::util::clock_hm(until));
    }
}

/// `bn queue resume --scope <s>` — lift a pause and start admitting again.
pub(crate) fn emit_resume(scope: &str, now: i64, json: bool) {
    let Some(parsed) = bn_core::queue::pause::PauseScope::parse(scope) else {
        fail_json(
            json,
            "queue_resume",
            "bad-scope",
            &format!("bn queue resume: unknown scope '{scope}'"),
        );
    };
    let since = match bn_core::queue::pause::resume(&parsed, now) {
        Ok(since) => since,
        Err(e) => fail_json(
            json,
            "queue_resume",
            "write-failed",
            &format!("bn queue resume: {e}"),
        ),
    };
    let key = parsed.key();
    if json {
        println!(
            "{}",
            Envelope::success(
                "queue_resume",
                // `resumed: false` is a real answer, not a failure: resuming something that was
                // never paused is a no-op the caller should be able to see.
                serde_json::json!({ "scope": key, "resumed": since.is_some(), "since": since }),
            )
            .to_json()
        );
        return;
    }
    match since {
        Some(since) => println!(
            "bn queue resume: {key} resumed — it was held for {}m",
            (now - since).max(0) / 60
        ),
        None => println!("bn queue resume: {key} was not paused"),
    }
}

/// `bn queue off [--for <duration>] [--reason <why>]` — stop filing work; run every trigger now.
///
/// The message says what the switch does *not* do, for the same reason `pause`'s does: "off" reads
/// as "stopped" to anyone who has not read the module doc, and here it means very nearly the
/// opposite — more starts sooner, not fewer.
pub(crate) fn emit_off(for_: Option<&str>, reason: &str, now: i64, json: bool) {
    let until = match for_ {
        Some(dur) => match bn_core::focus::parse_duration(dur).filter(|s| *s > 0) {
            Some(secs) => Some(now + secs),
            None => fail_json(
                json,
                "queue_off",
                "bad-duration",
                &format!("bn queue off: cannot read --for '{dur}' (try 90m, 2h)"),
            ),
        },
        None => None,
    };
    if let Err(e) = bn_core::queue::off::off(reason, until, now) {
        fail_json(json, "queue_off", "write-failed", &format!("bn queue off: {e}"));
    }
    if json {
        println!(
            "{}",
            Envelope::success(
                "queue_off",
                serde_json::json!({ "reason": reason, "until": until }),
            )
            .to_json()
        );
        return;
    }
    println!(
        "bn queue off: nothing is filed from now on — every trigger runs immediately, here"
    );
    match until {
        Some(until) => println!(
            "bn queue off: back on at {} (or `bn queue on`)",
            bn_core::util::clock_hm(until)
        ),
        None => println!("bn queue off: stays off until `bn queue on`"),
    }
    // Work filed before the switch is untouched and still drains, which is the question anybody
    // who just switched off with a full queue is about to ask.
    println!("bn queue off: work already queued is untouched and still drains");
}

/// `bn queue on` — file work for the dispatcher again.
pub(crate) fn emit_on(now: i64, json: bool) {
    let since = match bn_core::queue::off::on(now) {
        Ok(since) => since,
        Err(e) => fail_json(json, "queue_on", "write-failed", &format!("bn queue on: {e}")),
    };
    if json {
        println!(
            "{}",
            Envelope::success(
                "queue_on",
                // `switched: false` is a real answer, not a failure: switching on something that
                // was never off is a no-op the caller should be able to see.
                serde_json::json!({ "switched": since.is_some(), "since": since }),
            )
            .to_json()
        );
        return;
    }
    match since {
        Some(since) => println!(
            "bn queue on: triggers file work again — it was off for {}m",
            (now - since).max(0) / 60
        ),
        None => println!("bn queue on: the queue was already on"),
    }
}
