//! Starting one entry by hand, outside any pass — the door a person's direct command takes.
//!
//! The dispatcher's loop is built for the machine's own work: sample, plan, admit, place. A verb
//! someone just pressed in the picker does not want any of that; it wants to be running before
//! they look up. This module is the shortest honest path from "filed" to "leased and started"
//! that still leaves the ledger the dispatcher would have left, so the row, the event log and
//! `bn queue finish` cannot tell the two apart except by the rationale.
use super::{dispatch_fields, execute, finish_fields, Executed, LockDenied, TickLock};
use bn_core::backlog;
use bn_core::queue::tick::Dispatch;
use bn_core::resolver::Config;

/// Take the machine's tick lock for a direct start, waiting briefly for a pass to finish.
///
/// A pass is short — sampling, planning, a few appends — unless it is running express work inline,
/// which is bounded at five minutes. Three seconds covers the first case and refuses the second,
/// which is right: a person is waiting on the other side of this, and the fallback (file it and let
/// the pass start it) is what the queue does anyway.
pub(crate) fn hold_tick_lock() -> Result<TickLock, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        match TickLock::try_acquire() {
            Ok(guard) => return Ok(guard),
            Err(LockDenied::Unusable(e)) => {
                return Err(format!("cannot take the machine's queue lock — {e}"))
            }
            Err(LockDenied::Held) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(LockDenied::Held) => {
                return Err("a dispatcher pass is still deciding".to_string());
            }
        }
    }
}

/// How a direct start ended, when it did start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirectStart {
    /// Heavy work is running detached; `bn queue finish` will close the entry.
    Launched,
    /// Light work ran to completion here and the entry is already resolved with this code.
    Ran { code: i32 },
}

/// Start one pending entry right now, outside any pass — a person's direct command.
///
/// The whole admission policy in [`bn_core::queue::tick::plan_tick`] is skipped on purpose. That
/// policy exists to keep the *machine's* work — crons, warms, reclaims, agents — from
/// over-committing the box. A verb someone just pressed is not that: they are watching the row,
/// and every gate between them and the work (the heavy cap, a warm that owns the machine, a
/// pause, the next pass) reads as the keypress having failed. See
/// [`bn_core::resolver::DirectMode`].
///
/// What is *not* skipped is the ledger. The entry is leased, assigned and logged exactly as
/// [`super::realize`] would, and heavy work still resolves itself through `bn queue finish`, so
/// the row shows the same running/done states a dispatched entry does and `bn queue log` tells
/// the two apart only by the rationale.
///
/// The caller holds the tick lock (the `_held` guard is the proof) across filing *and* starting
/// the entry, so a dispatcher that woke on the append cannot plan the same entry in between — both
/// would lease it, and the build would run twice.
///
/// On any failure the lease is released and the entry is left pending for the dispatcher, which is
/// the fallback the person was promised: the queue handles what could not be started here.
pub(crate) fn dispatch_now(
    cfg: &Config,
    _held: &TickLock,
    entry: &bn_core::backlog::BacklogEntry,
    now: i64,
) -> Result<DirectStart, String> {
    let path = backlog::backlog_path();
    let d = Dispatch::direct(entry, "started by hand — a direct command skips admission");
    let ids = [d.id.clone()];
    let leased = backlog::lease_selected(&path, &d.repo, &ids, now)
        .map_err(|e| format!("could not lease {}/{}: {e}", d.repo, d.id))?;
    if leased.is_empty() {
        return Err("it is no longer pending — already started, or taken off the queue".to_string());
    }
    let _ = backlog::assign(&path, &d.repo, &d.id, cfg.machine(), now);
    let _ = bn_core::events::append_at("queue_dispatch", now, dispatch_fields(&d, cfg.machine()));
    // The same extent a pass declares, for the same reason: whatever `execute` re-enters must run
    // the work rather than file it again.
    let _dispatching = crate::cmd::queue_producers::Dispatching::begin();
    match execute(cfg, &d, now) {
        Executed::Launched => {
            let _ = backlog::clear_launch_failures(&path, &d.repo, &d.id);
            Ok(DirectStart::Launched)
        }
        // Not the path a picker verb takes — every one is heavy — but a light entry started by
        // hand still needs its ending written, exactly as `realize` writes it.
        Executed::Finished { code } => {
            let finished_at = bn_core::util::now_epoch_secs();
            let status = if code == 0 {
                bn_core::backlog::BacklogStatus::Done
            } else {
                bn_core::backlog::BacklogStatus::Failed
            };
            let _ = backlog::resolve_many(&path, &d.repo, &[(d.id.as_str(), status)], finished_at);
            let _ = bn_core::events::append_at(
                "queue_finish",
                finished_at,
                finish_fields(&d.repo, &d.id, code),
            );
            Ok(DirectStart::Ran { code })
        }
        Executed::Deferred(why) | Executed::Failed(why) | Executed::Refused(why) => {
            let refs = [d.id.as_str()];
            let _ = backlog::release_many(&path, &d.repo, &refs, bn_core::util::now_epoch_secs());
            Err(why)
        }
    }
}
