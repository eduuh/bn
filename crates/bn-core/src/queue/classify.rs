use crate::backlog::{BacklogEntry, ClassOverride, ExecSpec};

/// Whether a queue entry is user-led foreground work or autonomous machine work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueMode {
    /// Work a person normally wants to watch or review.
    Foreground,
    /// Work that can run without input.
    Autonomous,
    /// Autonomous work that should stay at the resource floor.
    Opportunistic,
}

impl QueueMode {
    /// Every mode, so a caller can enumerate or *validate* the label set rather than keeping its
    /// own copy of it. `bn-web` uses this to decide whether a `?class=` from a query string is a
    /// real class — which is what bounds its cache's key space.
    pub const ALL: [QueueMode; 3] = [
        QueueMode::Foreground,
        QueueMode::Autonomous,
        QueueMode::Opportunistic,
    ];

    pub fn label(self) -> &'static str {
        match self {
            QueueMode::Foreground => "foreground",
            QueueMode::Autonomous => "autonomous",
            QueueMode::Opportunistic => "opportunistic",
        }
    }

    pub fn is_autonomous(self) -> bool {
        matches!(self, QueueMode::Autonomous | QueueMode::Opportunistic)
    }
}

/// The kind of autonomous/background work, for filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueSubtype {
    Foreground,
    Cron,
    Warming,
    Recovery,
    Maintenance,
}

impl QueueSubtype {
    /// Every subtype — the companion to [`QueueMode::ALL`]; `?class=` matches against either set.
    pub const ALL: [QueueSubtype; 5] = [
        QueueSubtype::Foreground,
        QueueSubtype::Cron,
        QueueSubtype::Warming,
        QueueSubtype::Recovery,
        QueueSubtype::Maintenance,
    ];

    pub fn label(self) -> &'static str {
        match self {
            QueueSubtype::Foreground => "foreground",
            QueueSubtype::Cron => "cron",
            QueueSubtype::Warming => "warming",
            QueueSubtype::Recovery => "recovery",
            QueueSubtype::Maintenance => "maintenance",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueClass {
    pub mode: QueueMode,
    pub subtype: QueueSubtype,
}

/// Derive the queue class from durable entry data.
pub fn classify(entry: &BacklogEntry) -> QueueClass {
    if let Some(override_class) = entry.class_override {
        return match override_class {
            ClassOverride::Foreground => QueueClass {
                mode: QueueMode::Foreground,
                subtype: QueueSubtype::Foreground,
            },
            ClassOverride::Autonomous => QueueClass {
                mode: QueueMode::Autonomous,
                subtype: QueueSubtype::Maintenance,
            },
            ClassOverride::Opportunistic => QueueClass {
                mode: QueueMode::Opportunistic,
                subtype: QueueSubtype::Maintenance,
            },
        };
    }
    if entry.recovery {
        return QueueClass {
            mode: QueueMode::Autonomous,
            subtype: QueueSubtype::Recovery,
        };
    }
    if is_warming(entry) {
        return QueueClass {
            mode: QueueMode::Opportunistic,
            subtype: QueueSubtype::Warming,
        };
    }
    if entry.source.starts_with("cron:") || entry.kind == crate::queue::kind::BacklogKind::Cron {
        return QueueClass {
            mode: QueueMode::Autonomous,
            subtype: QueueSubtype::Cron,
        };
    }
    if is_machine(entry) {
        return QueueClass {
            mode: QueueMode::Autonomous,
            subtype: QueueSubtype::Maintenance,
        };
    }
    QueueClass {
        mode: QueueMode::Foreground,
        subtype: QueueSubtype::Foreground,
    }
}

/// Whether this entry is the machine's own bookkeeping rather than work a person chose to do.
///
/// The single predicate behind three otherwise-unrelated behaviours, which is why it is one function
/// and not three conditions written out three times:
///
/// - the minted task (#1001) is stamped `auto:1` and folds out of the human's actionable list;
/// - the entry classifies [`QueueMode::Autonomous`], so it stops competing in the watched lane;
/// - the SMART gate admits it without judgement — a prompt the machine wrote for itself has no
///   author to send it back to, so gating it only ever means never running it.
///
/// Two sources. What the entry *runs* answers it for jobs and shell commands: those launch no agent
/// and nobody typed them. Everything else is the producer's own declaration
/// ([`BacklogEntry::machine`]), because the same door serves both kinds — `bn dispatch` carries a
/// repo-setup prompt the dashboard generated and a sentence a person just typed, and only the caller
/// can tell them apart.
pub fn is_machine(entry: &BacklogEntry) -> bool {
    entry.machine || matches!(entry.exec, ExecSpec::Job { .. } | ExecSpec::Shell { .. })
}

/// Whether this entry is pool warming.
///
/// Read only from what the entry *declares* — the job it runs, or the cron that produced it. It
/// used to also substring-match `entry.item` for "warm", which is the entry's subject line: for a
/// job that is a short name, but for an agent it is the whole prompt. A repo-setup agent whose
/// prompt says to keep scripts idempotent for warm pools was therefore classified as pool warming,
/// made opportunistic, and held for an away window — priority-100 work a person had just clicked,
/// frozen by a word in its own instructions.
///
/// The job list lives in [`crate::queue::jobs::WARMING_JOBS`] so the source and exec answers cannot
/// drift apart.
pub fn is_warming(entry: &BacklogEntry) -> bool {
    let by_source = entry
        .source
        .strip_prefix("cron:")
        .is_some_and(crate::queue::jobs::is_warming);
    let by_exec = matches!(
        &entry.exec,
        ExecSpec::Job { job, .. } if crate::queue::jobs::is_warming(job)
    );
    by_source || by_exec
}

/// Whether this entry is the machine-wide mandatory pool refresh.
///
/// A targeted `pool-warm` comes from a person's per-repo dashboard action and remains ordinary
/// opportunistic maintenance. Only the untargeted daily sweep owns machine-wide admission.
///
/// Read from the **producer**, not from a missing repo. It used to match `repo: None` on the exec,
/// which worked only because the cron sweep happened to name no repo — an accident of
/// representation, and one that quietly inverted the moment crons started carrying the repo they
/// belong to. The two producers already declare themselves: the sweep enqueues with
/// `source = "cron:pool-warm"`, a person's dashboard warm with `source = "queue"`. Asking who
/// asked for the work is the question this function was always trying to answer.
pub fn is_mandatory_pool_warming(entry: &BacklogEntry) -> bool {
    crate::queue::jobs::is_scheduled_firing(&entry.source, "pool-warm")
        && matches!(&entry.exec, ExecSpec::Job { job, .. } if job == "pool-warm")
}

#[cfg(test)]
mod mandatory_pool_warming_tests {
    use super::*;
    use crate::backlog::{AddSpec, BacklogKind};

    fn pool_warm(source: &str, repo: Option<&str>) -> BacklogEntry {
        let spec = AddSpec {
            kind: BacklogKind::Cron,
            source: source.to_string(),
            exec: ExecSpec::Job {
                job: "pool-warm".to_string(),
                repo: repo.map(str::to_string),
                branch: None,
            },
            heavy: true,
            ..Default::default()
        };
        BacklogEntry::new("machine", "run pool-warm", &spec, 1000)
    }

    /// The regression this re-key exists for: the sweep now carries the repo it belongs to, and
    /// must still own machine-wide admission. Under the old `repo: None` match this returned false
    /// the moment crons started naming a repo — silently switching the whole lane off.
    #[test]
    fn the_cron_sweep_stays_mandatory_once_it_carries_a_repo() {
        assert!(is_mandatory_pool_warming(&pool_warm(
            "cron:pool-warm",
            Some("bn")
        )));
        assert!(is_mandatory_pool_warming(&pool_warm(
            "cron:pool-warm",
            None
        )));
    }

    /// A person's per-repo dashboard warm is ordinary opportunistic maintenance, whoever it names.
    #[test]
    fn a_targeted_dashboard_warm_is_not_mandatory() {
        assert!(!is_mandatory_pool_warming(&pool_warm(
            "queue",
            Some("arena")
        )));
        assert!(!is_mandatory_pool_warming(&pool_warm("queue", None)));
    }

    /// The source alone is not enough — a cron entry that runs something else is not pool warming.
    #[test]
    fn another_cron_job_is_not_mandatory_pool_warming() {
        let spec = AddSpec {
            kind: BacklogKind::Cron,
            source: "cron:pool-warm".to_string(),
            exec: ExecSpec::Job {
                job: "keep-warm".to_string(),
                repo: None,
                branch: None,
            },
            ..Default::default()
        };
        let entry = BacklogEntry::new("machine", "run keep-warm", &spec, 1000);
        assert!(!is_mandatory_pool_warming(&entry));
    }
}
