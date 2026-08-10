//! `bn queue pause` / `bn queue resume` — the drain.
//!
//! Its own module because `queue.rs` is at the 1000-line limit, and because these two are one
//! concern: what it means to hold work without stopping it. The policy they write into the event
//! log is read back by [`bn_core::queue::pause::PauseState`] and applied by the tick's admission
//! filter — nothing here decides anything, it only records the ask.
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
