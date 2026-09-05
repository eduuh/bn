//! Learned durations — how long the machine's work *actually* takes, read back from its own log.
//!
//! The queue has always carried an estimate ([`crate::backlog::BacklogEntry::est_secs`]) and has
//! almost never had one: producers do not set it, so every entry fell to a flat
//! [`DEFAULT_EST_SECS`] of twenty minutes. That default is not merely imprecise, it is wrong by
//! orders of magnitude in both directions — the median `attention-sync` run on the machine this was
//! written for takes **five seconds**, while a coding agent takes forty minutes. A projection built
//! on it is a drawing, not a forecast, which is why `bn queue timeline` would report a backlog of
//! thirty second-long crons as "beyond the horizon".
//!
//! Nothing had to be measured to fix that. [`crate::queue::log`] has been pairing every
//! `dispatch` with its `finish` into a [`Run`] carrying `duration_secs` all along; the numbers were
//! simply never read back. This module is that read-back, and it is deliberately the same shape as
//! [`crate::calib`], which already does the identical job for your own estimates against the punch
//! log: bucket the observations, take a **median**, and carry the sample count so a thin bucket is
//! visibly untrustworthy rather than quietly authoritative.
//!
//! Pure over the runs it is given — no clock, no disk — so the whole estimator is unit-testable and
//! the caller decides how much history to pay for.
use crate::backlog::BacklogEntry;
use crate::queue::log::Run;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The estimate for work nothing is known about.
///
/// Coarse on purpose: it places a mark on an axis, it does not promise a finish time. It is also
/// the *last* resort now rather than the usual answer, which is the whole point of this module.
pub const DEFAULT_EST_SECS: i64 = 20 * 60;

/// Runs a bucket needs before its median is trusted.
///
/// `calib` renders thin buckets with their `n` and lets a person judge them. This estimator feeds a
/// machine, and a machine cannot squint at a sample count — so the judgement is made here instead.
/// Three is the smallest count at which a median means anything at all: it can outvote one outlier.
pub const MIN_SAMPLES: usize = 3;

/// The longest single run allowed to teach an estimate.
///
/// Runs are paired by `(repo, id)`, and an id recurs — every firing of `run-watch` shares one. A
/// dispatch whose finish was never written therefore stays open until the *next* firing displaces
/// it, and a pairing that spans that gap measures the silence, not the work. Such a run is already
/// excluded by its non-terminal outcome; this is the second fence, for the case where a machine
/// slept mid-run and the clock disagrees with the work.
pub const MAX_PLAUSIBLE_SECS: i64 = 6 * 3600;

/// How far back to learn from.
///
/// Long enough that a twice-daily job clears [`MIN_SAMPLES`], short enough that the estimator
/// follows the machine rather than remembering it: a job that got three times faster last week
/// should say so this week.
pub const LEARN_WINDOW_SECS: i64 = 14 * 24 * 3600;

/// A ceiling on the rows folded in one pass, so a log that has never been rotated cannot turn an
/// estimate into an unbounded read. The window is the real bound; this is the backstop.
pub const LEARN_MAX_ROWS: usize = 50_000;

/// One learned bucket: the median duration observed, and how many runs it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sample {
    pub secs: i64,
    pub n: usize,
}

/// Where an estimate came from — carried onto the surfaces so "held because it needs 45m" can say
/// whether that 45m was declared by a producer or learned from the log. The difference matters: one
/// is an argument with the operator, the other an argument with the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The entry declared its own `est_secs`.
    Declared,
    /// Learned from this exact work's own history — the same repo and id.
    LearnedWork,
    /// Learned from other work of the same kind.
    LearnedKind,
    /// Nothing is known; [`DEFAULT_EST_SECS`].
    Default,
}

impl Source {
    /// The kebab-case label for the decision file and the status surfaces.
    pub fn label(self) -> &'static str {
        match self {
            Source::Declared => "declared",
            Source::LearnedWork => "learned-work",
            Source::LearnedKind => "learned-kind",
            Source::Default => "default",
        }
    }
}

/// An estimate and its provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Estimate {
    pub secs: i64,
    pub source: Source,
}

/// Observed durations, bucketed most-specific-first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Durations {
    buckets: BTreeMap<String, Sample>,
}

/// The bucket key for one piece of work — its `(repo, id)` pair.
///
/// The id is the job identity for everything that recurs: every firing of the `pool-warm` cron
/// enqueues `run-pool-warm`, so its runs accumulate into one bucket across days. One-shot work
/// (a `ship` entry, whose id is derived from its subject) lands in a bucket of one and is
/// correctly never trusted — [`MIN_SAMPLES`] sees to that without a special case.
fn work_key(repo: &str, id: &str) -> String {
    format!("work:{repo}/{id}")
}

fn kind_key(label: &str) -> String {
    format!("kind:{label}")
}

/// Median of a sample, in seconds. Sorts a local copy, so the caller's data is untouched.
///
/// A median rather than a mean, for the reason `calib` gives: one hung agent must not move the
/// estimate. It matters more here than there — `keep-warm` on the machine this was written for has
/// a median of 3s against a p90 of 304s, and a mean would describe neither.
///
/// Integer arithmetic, deliberately: these numbers reach the dispatcher's ordering, and a
/// projection that has to be bit-reproducible from a captured snapshot cannot go through a float.
fn median(v: &mut [i64]) -> i64 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        // Averaged the way the count implies, halving *after* the sum so an odd total does not
        // round two values apart into one that is neither.
        (v[n / 2 - 1] + v[n / 2]) / 2
    }
}

/// Whether a run is evidence of how long its work takes.
///
/// Only completions count. A failure measures how long the machine took to give up, an expiry
/// measures a window rather than any work, and an in-flight run has not finished so its "duration"
/// is just how long ago it started.
fn teaches(run: &Run) -> bool {
    run.outcome == "completed" && run.duration_secs > 0 && run.duration_secs <= MAX_PLAUSIBLE_SECS
}

impl Durations {
    /// Learn from a window of history.
    ///
    /// Pure: the caller chose how far back to read, and pays for it. Everything that is not a clean
    /// completion is dropped rather than averaged in — see [`teaches`].
    pub fn observe(runs: &[Run]) -> Durations {
        let mut work: BTreeMap<String, Vec<i64>> = BTreeMap::new();
        let mut kind: BTreeMap<String, Vec<i64>> = BTreeMap::new();
        for run in runs.iter().filter(|r| teaches(r)) {
            if !run.id.is_empty() {
                work.entry(work_key(&run.repo, &run.id))
                    .or_default()
                    .push(run.duration_secs);
            }
            if !run.work_kind.is_empty() {
                kind.entry(kind_key(&run.work_kind))
                    .or_default()
                    .push(run.duration_secs);
            }
        }
        let buckets = work
            .into_iter()
            .chain(kind)
            .filter(|(_, samples)| samples.len() >= MIN_SAMPLES)
            .map(|(key, mut samples)| {
                let n = samples.len();
                (
                    key,
                    Sample {
                        secs: median(&mut samples),
                        n,
                    },
                )
            })
            .collect();
        Durations { buckets }
    }

    /// What one bucket learned, if it cleared [`MIN_SAMPLES`].
    pub fn bucket(&self, key: &str) -> Option<Sample> {
        self.buckets.get(key).copied()
    }

    /// How many buckets carry a trusted median — the one number worth showing a person who wants
    /// to know whether the estimator has anything to say yet.
    pub fn len(&self) -> usize {
        self.buckets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }

    /// What the log knows about one piece of work, identified the way the log identifies it.
    ///
    /// Takes the identity rather than an entry, because the other half of the projection asks the
    /// same question about work that is *already running* — an [`crate::queue::tick::InFlight`]
    /// record, which is not a [`BacklogEntry`] and never will be.
    pub fn learned(&self, repo: &str, id: &str, kind_label: &str) -> Option<Estimate> {
        self.bucket(&work_key(repo, id))
            .map(|sample| Estimate {
                secs: sample.secs,
                source: Source::LearnedWork,
            })
            .or_else(|| {
                self.bucket(&kind_key(kind_label)).map(|sample| Estimate {
                    secs: sample.secs,
                    source: Source::LearnedKind,
                })
            })
    }

    /// The estimate for work identified by `(repo, id, kind)`, falling back to the flat default.
    pub fn for_work(&self, repo: &str, id: &str, kind_label: &str) -> Estimate {
        self.learned(repo, id, kind_label).unwrap_or(Estimate {
            secs: DEFAULT_EST_SECS,
            source: Source::Default,
        })
    }

    /// The estimate for `entry`, most specific first.
    ///
    /// A declared `est_secs` always wins: a producer that took the trouble to say how big its work
    /// is knows something the log cannot, and overriding it would make the field pointless. Below
    /// that, this work's own history beats its kind's, and the flat default is what remains when
    /// neither has enough runs to be worth believing.
    pub fn estimate(&self, entry: &BacklogEntry) -> Estimate {
        if entry.est_secs > 0 {
            return Estimate {
                secs: entry.est_secs,
                source: Source::Declared,
            };
        }
        self.for_work(&entry.repo, &entry.id, entry.kind.label())
    }

    /// The estimate in seconds — the projection's hot path, which does not care where it came from.
    pub fn for_entry(&self, entry: &BacklogEntry) -> i64 {
        self.estimate(entry).secs
    }
}

/// Learn from the machine's own recent history — the IO wrapper over [`Durations::observe`].
///
/// Split from the pure fold the same way [`crate::queue::log::read`] is split from
/// [`crate::queue::log::rows`]: the policy is testable without a state dir, and only this function
/// knows where the log lives or how much of it to pay for.
///
/// Bounded by [`LEARN_WINDOW_SECS`] and [`LEARN_MAX_ROWS`], because `events.jsonl` is append-only
/// and nothing rotates it. Callers on a hot path should cache the result rather than call this per
/// pass — today the only caller is the timeline, which a person asks for.
pub fn observe_recent(entries: &[BacklogEntry], now: i64) -> Durations {
    let rows = crate::queue::log::read(&crate::queue::log::Query {
        since: now.saturating_sub(LEARN_WINDOW_SECS),
        limit: LEARN_MAX_ROWS,
        kind: None,
        repo: None,
    });
    Durations::observe(&crate::queue::log::runs(&rows, entries, now))
}

// ------------------------------------------------------------------- the cache

/// How long a learned set is trusted before it is recomputed.
///
/// Durations move slowly — a job that took five seconds this morning takes five seconds this
/// afternoon — so this is generous. It exists to bound staleness, not to chase it.
pub const CACHE_MAX_AGE_SECS: i64 = 30 * 60;

/// Where the learned set is kept between passes.
pub fn cache_path() -> PathBuf {
    crate::resolver::state_dir().join("queue-durations.json")
}

/// The learned set plus when it was taken.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DurationsCache {
    generated_at: i64,
    durations: Durations,
}

/// Read the cached set, if it is fresh enough to trust.
///
/// A miss is never an error — an absent or unparseable cache simply means "not learned yet", and
/// the caller falls back to the flat default, which is exactly how the queue behaved before any of
/// this existed.
pub fn read_fresh_cache(now: i64) -> Option<Durations> {
    let text = std::fs::read_to_string(cache_path()).ok()?;
    let cache = serde_json::from_str::<DurationsCache>(&text).ok()?;
    (now.saturating_sub(cache.generated_at) <= CACHE_MAX_AGE_SECS).then_some(cache.durations)
}

fn write_cache(durations: &Durations, now: i64) -> std::io::Result<()> {
    let cache = DurationsCache {
        generated_at: now,
        durations: durations.clone(),
    };
    crate::store::save_json_atomic(&cache_path(), &cache)
}

/// The learned set for this pass: the cache when it is fresh, else a recompute that warms it.
///
/// This is the function the dispatcher calls, and the reason the cache exists at all. `events.jsonl`
/// is append-only with no rotation — 49 MB and 193,000 lines on the machine this was written for —
/// and the resident tick runs every three seconds. Folding the whole log on every pass to answer
/// "how long does this take" would cost far more than the answer is worth. Recomputing twice an
/// hour costs nothing and is more than accurate enough for a number that changes on the scale of
/// weeks.
///
/// A failed write is not fatal: the estimate is still correct for this pass, and the next one
/// recomputes. Reporting it would turn a slow disk into a dispatcher error.
pub fn cached_or_compute(entries: &[BacklogEntry], now: i64) -> Durations {
    if let Some(cached) = read_fresh_cache(now) {
        return cached;
    }
    let learned = observe_recent(entries, now);
    let _ = write_cache(&learned, now);
    learned
}
