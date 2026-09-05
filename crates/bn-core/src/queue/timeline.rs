//! The day timeline: what ran, what is running, what is scheduled, and what is projected to start.
//!
//! One pure fold, so `bn queue timeline` and `GET /api/queue/timeline` build the identical struct
//! and cannot drift into telling you different stories about the same day.
//!
//! The load-bearing decision is how a **queued** entry gets a time at all. It has none — priority
//! says what runs before what, not when. The answer is to replay [`crate::queue::tick::plan_tick`]
//! forward: run the tick, assume each started item holds its lane for its estimated duration,
//! advance to the earliest completion, repeat. That makes the projection the *same policy the
//! dispatcher will actually apply* rather than a second model of it — which is the only version
//! worth drawing, because a projection that disagrees with the dispatcher is worse than no
//! projection.
//!
//! It is deliberately coarse, and says so: every projected mark carries the `wave` it lands in
//! alongside its clock estimate, and is drawn hollow. An entry that does not fit the horizon is
//! counted, never drawn — a projection crammed against the right edge reads as a promise.
use crate::backlog::BacklogEntry;
use crate::queue::classify::{self, QueueClass, QueueMode, QueueSubtype};
use crate::queue::kind::{BacklogKind, WorkClass};
use crate::queue::tick::{plan_tick, InFlight, TickInputs};
use serde::Serialize;

use crate::queue::durations::Durations;

/// One entry's projected start.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Projected {
    pub repo: String,
    pub id: String,
    /// Local-epoch projected start, or `None` when it does not fit the horizon.
    pub at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// The 0-based wave it lands in — the honest signal when the clock estimate is soft.
    pub wave: usize,
    /// Why it is not in wave 0.
    pub note: String,
}

/// Replay the tick forward to project when each pending entry would start.
///
/// Each round runs the real policy, marks what it started as in-flight for that item's estimated
/// duration, then advances the simulated clock to the earliest completion or snooze expiry so the
/// next round sees the freed capacity / newly-eligible work. Bounded by `horizon_secs` and by
/// rounds, so a permanently-unplaceable entry (no capable node) ends the loop rather than spinning.
pub fn project(inputs: &TickInputs, est: &Durations, horizon_secs: i64) -> Vec<Projected> {
    let mut out: Vec<Projected> = Vec::new();
    let mut remaining: Vec<BacklogEntry> = inputs.pending.to_vec();
    let mut clock = inputs.now;
    let deadline = inputs.now + horizon_secs;
    let mut wave = 0usize;

    // Everything occupying capacity right now, with the instant it is assumed to free up.
    //
    // An agent that is already running has no declared end — nobody knows when it finishes. Left
    // un-retired it would hold its lane for the whole horizon, which makes every coding entry read
    // as "beyond the horizon" forever: technically defensible, useless in practice. Assuming it
    // runs a typical duration from when it *started* is a guess, and the projection is already
    // labelled a guess.
    let mut busy: Vec<(InFlight, i64)> = inputs
        .in_flight
        .iter()
        .map(|f| {
            // Asked of the log the same way a pending entry's is, so the two halves of the replay
            // agree. A running job whose history says it takes five seconds should not hold its
            // lane for twenty minutes just because nobody wrote down when it would finish.
            let ends = f.started_at + est.for_work(&f.repo, &f.id, f.kind.label()).secs;
            (f.clone(), ends.max(inputs.now + 60))
        })
        .collect();

    while !remaining.is_empty() && clock <= deadline && wave < 64 {
        let in_flight: Vec<InFlight> = busy.iter().map(|(f, _)| f.clone()).collect();
        let round = TickInputs {
            now: clock,
            pending: &remaining,
            in_flight: &in_flight,
            ..inputs.clone()
        };
        let plan = plan_tick(&round);
        for d in &plan.start {
            let Some(entry) = remaining.iter().find(|e| e.repo == d.repo && e.id == d.id) else {
                continue;
            };
            busy.push((
                InFlight {
                    repo: d.repo.clone(),
                    id: d.id.clone(),
                    kind: d.kind,
                    work_class: d.work_class,
                    heavy: d.heavy,
                    // The replay just started it, so the replay owns it.
                    queue_owned: true,
                    node: Some(d.target.label()),
                    touches: entry.touches.clone(),
                    started_at: clock,
                },
                clock + est.for_entry(entry),
            ));
            out.push(Projected {
                repo: d.repo.clone(),
                id: d.id.clone(),
                at: Some(clock),
                node: Some(d.target.label()),
                wave,
                note: d.rationale.clone(),
            });
        }
        remaining.retain(|e| !out.iter().any(|p| p.repo == e.repo && p.id == e.id));
        if remaining.is_empty() {
            break;
        }
        // Advance to the next moment capacity frees up or a snoozed task becomes eligible. No
        // future release/expiry means nothing later can start either, so stop rather than spin.
        let next_release = busy
            .iter()
            .map(|(_, ends)| *ends)
            .filter(|t| *t > clock)
            .min();
        let next_unsnooze = remaining
            .iter()
            .filter_map(|e| e.not_before)
            .filter(|t| *t > clock)
            .min();
        let next = match (next_release, next_unsnooze) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        let Some(next) = next else {
            break;
        };
        clock = next;
        busy.retain(|(_, ends)| *ends > clock);
        wave += 1;
    }

    // Whatever never got a wave does not fit the horizon; counted, not drawn.
    for entry in &remaining {
        out.push(Projected {
            repo: entry.repo.clone(),
            id: entry.id.clone(),
            at: None,
            node: None,
            wave,
            note: "beyond the horizon at projected throughput".to_string(),
        });
    }
    out
}

/// The day-timeline viewmodel.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Timeline {
    /// The local date shown, `YYYY-MM-DD`.
    pub date: String,
    /// Local-epoch window bounds — midnight to midnight.
    pub start: i64,
    pub end: i64,
    /// Server-truth "now", so the client draws the current-time rule from it rather than from a
    /// browser clock that may be minutes off.
    pub now: i64,
    /// The bands, in draw order.
    pub bands: Vec<Band>,
    /// Per-hour occupancy, 24 entries — the density strip a narrow layout degrades to.
    pub load: Vec<HourLoad>,
    /// Pending entries that do not fit the window at projected throughput.
    pub beyond: usize,
}

/// A horizontal band. Separate rows rather than one merged track, so "what is running" can be read
/// without untangling it from "what is merely planned".
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Band {
    pub id: BandId,
    pub label: String,
    pub marks: Vec<Mark>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BandId {
    /// Alive right now — spans from `started_at`, open-ended on the right.
    Running,
    /// Scheduled agents with a declared clock slot — point marks at their `on_calendar` times.
    Scheduled,
    /// Queued entries with no clock — point marks at their projected start, drawn hollow.
    Projected,
    /// Finished today — spans, dimmed.
    Done,
}

impl BandId {
    pub fn label(self) -> &'static str {
        match self {
            BandId::Running => "Running",
            BandId::Scheduled => "Scheduled",
            BandId::Projected => "Projected",
            BandId::Done => "Done",
        }
    }
}

/// One thing on the timeline.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Mark {
    /// `<repo>/<id>` or a job name — the React key and the click-through target.
    pub key: String,
    pub label: String,
    /// The kebab-case kind label — the colour key.
    pub kind: String,
    /// `coding` | `research` — the second encoding axis.
    pub lane: String,
    pub priority: i64,
    /// Local-epoch start. For a point mark, `end == at`.
    pub at: i64,
    pub end: i64,
    /// Whether `at` is a **projection**. Drawn dashed and hollow: a projected time rendered
    /// identically to a scheduled one is a lie the UI tells every time it turns out wrong.
    pub projected: bool,
    /// The tooltip: the rationale, the `on_calendar` spec, or the defer reason.
    pub note: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    pub mode: String,
    pub subtype: String,
    pub autonomous: bool,
    pub opportunistic: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HourLoad {
    pub hour: u32,
    pub running: usize,
    pub scheduled: usize,
    pub projected: usize,
}

/// The inputs to [`build`]. Everything is passed in — no clock, no disk — so the fold is
/// deterministic for a given day and reproducible from a captured snapshot.
pub struct TimelineInputs<'a> {
    pub now: i64,
    /// The local day to render, `YYYY-MM-DD`. A string rather than a `NaiveDate` so bn-cli — which
    /// deliberately carries no chrono dependency — can build one without acquiring the whole
    /// calendar library to name a day.
    pub date: &'a str,
    /// Declared crons and Markdown agents with `on_calendar` — the Scheduled band's source.
    pub catalog: &'a [(String, Option<String>)],
    /// All backlog entries, including leased and terminal rows, for classifying running marks.
    pub entries: &'a [BacklogEntry],
    pub pending: &'a [BacklogEntry],
    pub in_flight: &'a [InFlight],
    pub done_today: &'a [BacklogEntry],
    pub projection: &'a [Projected],
    /// `foreground`, `autonomous`, `opportunistic`, `cron`, `warming`, `recovery`, or
    /// `maintenance`. Empty/unknown means no filter.
    pub class_filter: Option<&'a str>,
}

/// Fold the day into bands.
pub fn build(inputs: &TimelineInputs) -> Timeline {
    let date = parse_date(inputs.date).unwrap_or_else(|| local_date(inputs.now));
    let (start, end) = day_bounds(date);
    let class_filter = normalize_class_filter(inputs.class_filter);
    let mut bands = Vec::new();

    bands.push(Band {
        id: BandId::Running,
        label: BandId::Running.label().to_string(),
        marks: inputs
            .in_flight
            .iter()
            .filter_map(|f| {
                let class = inputs
                    .entries
                    .iter()
                    .find(|e| e.repo == f.repo && e.id == f.id)
                    .map(classify::classify)
                    .unwrap_or_else(|| class_for_kind(f.kind));
                if !class_matches(class, class_filter) {
                    return None;
                }
                Some((f, class))
            })
            .map(|(f, class)| Mark {
                key: format!("{}/{}", f.repo, f.id),
                label: f.id.clone(),
                kind: f.kind.label().to_string(),
                lane: f.work_class.label().to_string(),
                priority: 0,
                at: f.started_at.max(start),
                end: inputs.now,
                projected: false,
                note: "running now".to_string(),
                node: f.node.clone(),
                mode: class.mode.label().to_string(),
                subtype: class.subtype.label().to_string(),
                autonomous: class.mode.is_autonomous(),
                opportunistic: class.mode == QueueMode::Opportunistic,
            })
            .collect(),
    });

    // A captured job is no longer a *claim* about the future — it is real queued work, and the
    // Projected band already draws it at the time it would actually start. Drawing it in both
    // places double-counts it and, worse, keeps asserting a scheduled time the queue may not hit.
    let queued_jobs: Vec<&str> = inputs
        .pending
        .iter()
        .filter_map(|e| e.source.strip_prefix("cron:"))
        .collect();
    bands.push(Band {
        id: BandId::Scheduled,
        label: BandId::Scheduled.label().to_string(),
        marks: inputs
            .catalog
            .iter()
            .filter(|(job, _)| !queued_jobs.contains(&job.as_str()))
            .flat_map(|(job, schedule)| {
                let spec = schedule.clone().unwrap_or_default();
                crate::staff_schedule::slots_on(&spec, date)
                    .into_iter()
                    .filter(|_| {
                        class_matches(
                            QueueClass {
                                mode: QueueMode::Autonomous,
                                subtype: QueueSubtype::Cron,
                            },
                            class_filter,
                        )
                    })
                    .map(move |at| Mark {
                        key: job.clone(),
                        label: job.clone(),
                        kind: BacklogKind::Cron.label().to_string(),
                        lane: WorkClass::Research.label().to_string(),
                        priority: 0,
                        at,
                        end: at,
                        projected: false,
                        note: spec.clone(),
                        node: None,
                        mode: QueueMode::Autonomous.label().to_string(),
                        subtype: QueueSubtype::Cron.label().to_string(),
                        autonomous: true,
                        opportunistic: false,
                    })
                    .collect::<Vec<_>>()
            })
            .collect(),
    });

    let projected: Vec<Mark> = inputs
        .projection
        .iter()
        .filter_map(|p| {
            let at = p.at?;
            let entry = inputs
                .pending
                .iter()
                .find(|e| e.repo == p.repo && e.id == p.id)?;
            Some(Mark {
                mode: {
                    let class = classify::classify(entry);
                    class.mode.label().to_string()
                },
                subtype: {
                    let class = classify::classify(entry);
                    class.subtype.label().to_string()
                },
                autonomous: classify::classify(entry).mode.is_autonomous(),
                opportunistic: classify::classify(entry).mode == QueueMode::Opportunistic,
                key: format!("{}/{}", p.repo, p.id),
                label: entry.item.clone(),
                kind: entry.kind.label().to_string(),
                lane: WorkClass::from_kind(entry.kind).label().to_string(),
                priority: entry.priority,
                at,
                end: at,
                projected: true,
                note: format!("wave {} — {}", p.wave, p.note),
                node: p.node.clone(),
            })
        })
        .filter(|m| mark_matches(m, class_filter))
        .collect();
    bands.push(Band {
        id: BandId::Projected,
        label: BandId::Projected.label().to_string(),
        marks: projected,
    });

    bands.push(Band {
        id: BandId::Done,
        label: BandId::Done.label().to_string(),
        marks: inputs
            .done_today
            .iter()
            .filter(|e| class_matches(classify::classify(e), class_filter))
            .map(|e| Mark {
                mode: classify::classify(e).mode.label().to_string(),
                subtype: classify::classify(e).subtype.label().to_string(),
                autonomous: classify::classify(e).mode.is_autonomous(),
                opportunistic: classify::classify(e).mode == QueueMode::Opportunistic,
                key: format!("{}/{}", e.repo, e.id),
                label: e.item.clone(),
                kind: e.kind.label().to_string(),
                lane: WorkClass::from_kind(e.kind).label().to_string(),
                priority: e.priority,
                at: e.updated_at.max(start),
                end: e.updated_at.max(start),
                projected: false,
                note: "finished".to_string(),
                node: e.assigned_to.clone(),
            })
            .collect(),
    });

    Timeline {
        date: date.format("%Y-%m-%d").to_string(),
        start,
        end,
        now: inputs.now,
        load: hour_load(&bands, start),
        beyond: inputs
            .projection
            .iter()
            .filter(|p| p.at.is_none())
            .filter(|p| {
                inputs
                    .pending
                    .iter()
                    .find(|e| e.repo == p.repo && e.id == p.id)
                    .is_some_and(|e| class_matches(classify::classify(e), class_filter))
            })
            .count(),
        bands,
    }
}

fn class_for_kind(kind: BacklogKind) -> QueueClass {
    if kind == BacklogKind::Cron {
        return QueueClass {
            mode: QueueMode::Autonomous,
            subtype: QueueSubtype::Cron,
        };
    }
    QueueClass {
        mode: QueueMode::Foreground,
        subtype: QueueSubtype::Foreground,
    }
}

fn normalize_class_filter(filter: Option<&str>) -> Option<&str> {
    match filter
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "all")
    {
        Some(
            raw @ ("foreground" | "autonomous" | "opportunistic" | "cron" | "warming" | "recovery"
            | "maintenance"),
        ) => Some(raw),
        _ => None,
    }
}

fn class_matches(class: QueueClass, filter: Option<&str>) -> bool {
    filter.is_none_or(|want| class.mode.label() == want || class.subtype.label() == want)
}

fn mark_matches(mark: &Mark, filter: Option<&str>) -> bool {
    filter.is_none_or(|want| mark.mode == want || mark.subtype == want)
}

/// Parse a `YYYY-MM-DD` local date.
pub fn parse_date(s: &str) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()
}

/// The local date `now` falls on.
pub fn local_date(now: i64) -> chrono::NaiveDate {
    chrono::DateTime::from_timestamp(now, 0)
        .map(|dt| dt.with_timezone(&chrono::Local).date_naive())
        .unwrap_or_default()
}

/// Local midnight-to-midnight bounds for `date`.
fn day_bounds(date: chrono::NaiveDate) -> (i64, i64) {
    use chrono::TimeZone;
    let start = date
        .and_hms_opt(0, 0, 0)
        .and_then(|d| chrono::Local.from_local_datetime(&d).earliest())
        .map(|d| d.timestamp())
        .unwrap_or_default();
    (start, start + 86_400)
}

/// Per-hour occupancy — the strip a narrow layout degrades to when a Gantt would be unreadable.
fn hour_load(bands: &[Band], start: i64) -> Vec<HourLoad> {
    let mut load: Vec<HourLoad> = (0..24)
        .map(|hour| HourLoad {
            hour,
            running: 0,
            scheduled: 0,
            projected: 0,
        })
        .collect();
    for band in bands {
        for mark in &band.marks {
            let hour = ((mark.at - start) / 3600).clamp(0, 23) as usize;
            match band.id {
                BandId::Running => load[hour].running += 1,
                BandId::Scheduled => load[hour].scheduled += 1,
                BandId::Projected => load[hour].projected += 1,
                BandId::Done => {}
            }
        }
    }
    load
}

/// Build the day's timeline from live state, using the **persisted** node registry.
///
/// In bn-core so the CLI and the web server share one assembly rather than each growing their own.
/// It reads the registry the tick maintains instead of re-polling nodes over SSH: a web request
/// must not block on a network round-trip to a box that may be down, and the tick has already
/// answered that question within the last minute.
pub fn build_live(
    cfg: &crate::resolver::Config,
    date: &str,
    now: i64,
    class_filter: Option<&str>,
) -> Result<Timeline, crate::agents::AgentCatalogError> {
    let snap = crate::queue::snapshot::Snapshot::capture(cfg, now);
    // Learned from the machine's own log rather than assumed, and taken from the snapshot so the
    // projection and the dispatcher that produced it are working from the same numbers. Before
    // this the projection ran on a flat twenty-minute guess for every entry, which is why a
    // backlog of second-long crons drew as "beyond the horizon".
    let projection = project(&snap.inputs(), &snap.durations, 12 * 3600);
    let automation = crate::action_center::build_todays_automation(cfg, None)?;
    let catalog: Vec<(String, Option<String>)> = automation
        .crons
        .iter()
        .chain(automation.agents.iter())
        .map(|c| (c.name.clone(), c.schedule.clone()))
        .collect();
    Ok(build(&TimelineInputs {
        now,
        date,
        catalog: &catalog,
        entries: &snap.entries,
        pending: &snap.pending,
        in_flight: &snap.in_flight,
        done_today: &snap.done_on(date),
        projection: &projection,
        class_filter,
    }))
}
