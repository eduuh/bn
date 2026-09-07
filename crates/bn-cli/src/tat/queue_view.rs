//! What the queue says about one repo's verbs — the picker's read-only view of the dispatcher.
//!
//! Split out of [`super::commands`] because it is a different job: that module owns the rows, the
//! keys and the nested action picker, this one owns reading two files and answering "what is this
//! verb doing, and why". The split also keeps both under the 1000-line ratchet, whose rule is that
//! a file needing to grow is split rather than compressed.
//!
//! **Everything here runs on the picker's hot path**, which re-execs `bn` once per cursor move and
//! repaints every 700ms. Two reads, both deliberate: the folded backlog once per paint (it answers
//! for every verb at once), and `queue-decision.json`, which is about two kilobytes. Nothing here
//! may reach for `bn queue timeline` — a better answer to the same question, and far too expensive:
//! `build_live` goes through `Snapshot::capture` to `agent::supervise`, which shells out to tmux and
//! reads the whole metrics log.
/// What the queue says about one verb right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CmdState {
    /// Running, and for how long. The row spins.
    Running(i64),
    /// Filed to run now, still waiting. `held` carries the dispatcher's own reason when it has
    /// recorded one, and how long the entry has been waiting.
    ///
    /// A bare "queued" is the least useful thing a row can say, because it is true of work that is
    /// about to start and of work that will never start. The reason is what separates them, and the
    /// dispatcher already writes it — see [`held_reasons`].
    Queued(Option<Held>),
    /// Filed for the away window.
    Away,
}

/// Why one entry is not running, as the last dispatcher pass recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Held {
    /// The kebab-case deferral label — `capacity`, `paused`, `awaiting-away`, `mandatory-warming`.
    pub(crate) reason: String,
    /// Seconds it has been waiting, or `None` when the record does not say.
    pub(crate) waiting_secs: Option<i64>,
}

/// The queue state of every verb, keyed by action id.
///
/// Read once per paint rather than per row: one backlog load answers for the whole section.
/// Deliberately reports work **this picker did not start** too — a `pool-warm` the cron began is
/// still a pool warm, and a row that only admitted to your own keypresses would be lying about what
/// the machine is doing.
pub(crate) type CmdStates = std::collections::BTreeMap<String, CmdState>;

/// Why each entry the last pass deferred is waiting, keyed by entry id.
///
/// From `queue-decision.json`, which the dispatcher rewrites on **every** pass — including the ones
/// that start nothing, which are exactly the passes a stuck row needs explained. It is about two
/// kilobytes and already on disk, so this costs one small read.
///
/// That cheapness is the whole reason the reasons come from here and not from `bn queue timeline`,
/// which would answer the same question better: `timeline::build_live` reaches
/// `Snapshot::capture` → `agent::supervise`, which shells out to tmux and reads the whole metrics
/// log. This runs on every paint of a picker whose side pane re-execs per keystroke.
pub(crate) fn held_reasons() -> std::collections::HashMap<String, Held> {
    let Some(decision) = bn_core::queue::decision::QueueDecision::read() else {
        return Default::default();
    };
    let now = bn_core::util::now_epoch_secs();
    decision
        .deferred
        .into_iter()
        .map(|row| {
            let waiting = (row.waiting_since > 0).then(|| (now - row.waiting_since).max(0));
            (
                row.id,
                Held {
                    // The detail is a sentence; a row has one short column. The label is the
                    // vocabulary `bn queue why` already uses, so the two surfaces agree.
                    reason: row.reason,
                    waiting_secs: waiting,
                },
            )
        })
        .collect()
}

/// Build [`CmdStates`] for `repo` from the backlog.
pub(crate) fn command_states(repo: &str, now: i64) -> CmdStates {
    use bn_core::backlog::{BacklogStatus, ExecSpec};
    let backlog = bn_core::backlog::load(&bn_core::backlog::backlog_path());
    let held = held_reasons();
    let mut out = CmdStates::new();
    for entry in backlog.entries() {
        if !entry.status.is_active() {
            continue;
        }
        // The entry's own `repo` is the queue's home (`machine`); the repo the work *targets* is
        // inside the exec, which is what a row is asking about.
        let ExecSpec::Job {
            job, repo: target, ..
        } = &entry.exec
        else {
            continue;
        };
        if target.as_deref() != Some(repo) {
            continue;
        }
        // A job the roster does not know is one of the repo's own scripts, keyed the way its row
        // is (`script:<name>`, see `commands::scripts`). Keyed rather than dropped so a script row
        // can spin; the caller trims keys no row was drawn for.
        let key = match bn_core::wt_actions::all().iter().find(|a| a.job == *job) {
            Some(action) => action.id.to_string(),
            None => super::commands::scripts::id(job),
        };
        let state = match entry.status {
            // `updated_at` is stamped when the entry is leased, so on a running entry it is the
            // start time — no separate clock to keep.
            BacklogStatus::Leased => CmdState::Running((now - entry.updated_at).max(0)),
            _ if entry.class_override == Some(bn_core::backlog::ClassOverride::Opportunistic) => {
                CmdState::Away
            }
            _ => CmdState::Queued(held.get(&entry.id).cloned()),
        };
        out.insert(key, state);
    }
    out
}
