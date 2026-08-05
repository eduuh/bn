//! Lifecycle metrics over a trailing window, and work-status classification of
//! started items against live agent windows.

use super::model::AttnItem;
use super::query::StartedItem;
use super::store::field_str;
use crate::events;
use serde::Serialize;
use serde_json::Value;

/// A rollup of the attention lifecycle over a trailing window — how much work the list took on and
/// cleared, and how long it sat in-flight. Read from the local event log (the `attn_*` events),
/// distinct from the synced PR scorecard.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct LifecycleMetrics {
    pub window_days: i64,
    /// Items raised in the window, split by the mode they were raised with.
    pub raised: usize,
    pub raised_manual: usize,
    pub raised_assist: usize,
    pub raised_auto: usize,
    /// Lifecycle transitions in the window.
    pub started: usize,
    pub done: usize,
    pub acked: usize,
    /// Time-in-flight (started → done) over items *completed* in the window, in seconds.
    pub avg_inflight_secs: Option<i64>,
    pub median_inflight_secs: Option<i64>,
}

/// Fold the attention event log into a [`LifecycleMetrics`] over the trailing `window_days`. Time-
/// in-flight pairs each `attn_done` with the item's latest preceding `attn_started` (events are
/// chronological), so a restart counts from the most recent launch.
pub fn lifecycle_metrics(now: i64, window_days: i64) -> LifecycleMetrics {
    let cutoff = now - window_days.max(0) * 86_400;
    let events = events::read_all();
    let mut m = LifecycleMetrics {
        window_days,
        ..Default::default()
    };
    let mut started_at: std::collections::HashMap<String, i64> = Default::default();
    let mut inflight: Vec<i64> = Vec::new();
    for ev in &events {
        let in_window = ev.ts >= cutoff;
        match ev.kind.as_str() {
            "attn" if in_window => {
                m.raised += 1;
                match ev.fields.get("mode").and_then(Value::as_str) {
                    Some("auto") => m.raised_auto += 1,
                    Some("assist") => m.raised_assist += 1,
                    _ => m.raised_manual += 1,
                }
            }
            "attn_started" => {
                if let Some(r) = field_str(ev, "ref") {
                    started_at.insert(r, ev.ts);
                }
                if in_window {
                    m.started += 1;
                }
            }
            "attn_done" => {
                if in_window {
                    m.done += 1;
                    if let Some(started) = field_str(ev, "ref").and_then(|r| started_at.get(&r)) {
                        inflight.push((ev.ts - started).max(0));
                    }
                }
            }
            "attn_ack" if in_window => m.acked += 1,
            _ => {}
        }
    }
    if !inflight.is_empty() {
        let sum: i64 = inflight.iter().sum();
        m.avg_inflight_secs = Some(sum / inflight.len() as i64);
        inflight.sort_unstable();
        m.median_inflight_secs = Some(inflight[inflight.len() / 2]);
    }
    m
}

/// An item just `mark_started` shows `launching` until its window + agent spin up — this grace
/// window keeps it from bouncing to `orphaned` before tmux and the harness appear (R3).
pub const START_GRACE_SECS: i64 = 180;

/// The live work status of an in-flight attention item — derived, never stored, by joining the
/// item to its tmux window + agent. Cheap by design (R1): the window fields come from the single
/// `list-windows -a` the picker already makes; there is no transcript scan on this path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkStatus {
    /// Started, but its window/agent hasn't appeared yet (within the grace window).
    Launching,
    /// Agent (or you) actively working — recent activity.
    Working,
    /// Working state gone quiet past the idle threshold — thinking or between turns.
    Idle,
    /// Quiet past the stuck threshold — probably wedged; needs a nudge.
    Stuck,
    /// Agent stopped and is awaiting your input.
    Waiting,
    /// Finished, awaiting your review (an `attn_done`, or the window's own done flag).
    Done,
    /// The backing note has open blockers.
    Blocked,
    /// Started long ago with no window and no agent — the run died or never launched.
    Orphaned,
}

/// Which lane an in-flight item renders in on the three-lane board (step 3). `Orphaned` folds back
/// to the to-start lane (its run is gone); the rest split into needs-you interrupts vs healthy
/// in-progress work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    NeedsYou,
    InProgress,
    ToStart,
}

impl WorkStatus {
    /// The lowercase status label for badges / JSON.
    pub fn label(self) -> &'static str {
        match self {
            WorkStatus::Launching => "launching",
            WorkStatus::Working => "working",
            WorkStatus::Idle => "idle",
            WorkStatus::Stuck => "stuck",
            WorkStatus::Waiting => "waiting",
            WorkStatus::Done => "done",
            WorkStatus::Blocked => "blocked",
            WorkStatus::Orphaned => "orphaned",
        }
    }

    /// Whether this status is a needs-you interrupt (done / stuck / waiting / blocked) vs healthy
    /// in-progress work.
    pub fn needs_you(self) -> bool {
        self.lane() == Lane::NeedsYou
    }

    /// The board lane for this status.
    pub fn lane(self) -> Lane {
        match self {
            WorkStatus::Done | WorkStatus::Stuck | WorkStatus::Waiting | WorkStatus::Blocked => {
                Lane::NeedsYou
            }
            WorkStatus::Working | WorkStatus::Idle | WorkStatus::Launching => Lane::InProgress,
            WorkStatus::Orphaned => Lane::ToStart,
        }
    }
}

/// A snapshot of the tmux window backing an in-flight item, joined by `repo:branch`. Built by the
/// CLI from `@bn_agent` / `@bn_agent_since` / `@bn_done` / `@bn_blockers`; `None` when no window
/// exists for the item.
#[derive(Debug, Clone, Default)]
pub struct WindowState {
    /// `@bn_agent`: "working" | "waiting" | "done" | "" (unknown / not a Claude-driven window).
    pub agent: String,
    /// `@bn_agent_since` epoch secs — when the window entered `agent` state (0 if unknown).
    pub agent_since: i64,
    /// `@bn_done` set — the silence-watch "finished" flag (a non-Claude window's done signal).
    pub done_flag: bool,
    /// `@bn_blockers` non-empty — the backing note has open blockers.
    pub blockers: bool,
}

/// Classify one in-flight item's live status. Pure over its inputs — the classic bn classifier
/// shape (cf. `jump::window_row`). Precedence: an explicit `attn_done` wins; then, with no window,
/// grace decides launching vs orphaned; otherwise a real agent signal (waiting/working/done) is
/// trusted, falling back to the window's done/blocker flags when the agent state is unknown.
pub fn work_status(
    done: bool,
    since: i64,
    window: Option<&WindowState>,
    now: i64,
    idle_mins: i64,
    stuck_mins: i64,
    grace_secs: i64,
) -> WorkStatus {
    if done {
        return WorkStatus::Done;
    }
    let Some(w) = window else {
        return if now - since <= grace_secs {
            WorkStatus::Launching
        } else {
            WorkStatus::Orphaned
        };
    };
    match w.agent.as_str() {
        "waiting" => WorkStatus::Waiting,
        "done" => WorkStatus::Done,
        // A real "working" agent is the source of truth — age it for idle/stuck, ignoring any
        // stale blocker/done flags on the window.
        "working" => working_age(w.agent_since, now, idle_mins, stuck_mins),
        // No agent signal (manual/assist window, or hooks unwired): fall back to the window flags.
        _ => {
            if w.done_flag {
                WorkStatus::Done
            } else if w.blockers {
                WorkStatus::Blocked
            } else {
                WorkStatus::Working
            }
        }
    }
}

/// Age a `working` window into working/idle/stuck from how long it has sat in that state
/// (`@bn_agent_since`). A missing/zero `since` can't be aged → `working`.
fn working_age(agent_since: i64, now: i64, idle_mins: i64, stuck_mins: i64) -> WorkStatus {
    if agent_since <= 0 {
        return WorkStatus::Working;
    }
    let idle = (now - agent_since).max(0);
    if idle >= stuck_mins * 60 {
        WorkStatus::Stuck
    } else if idle >= idle_mins * 60 {
        WorkStatus::Idle
    } else {
        WorkStatus::Working
    }
}

/// The canonical `repo:branch` tmux-window key: repo's last path segment lowercased (so an
/// `owner/name` locator matches the session named for the repo dir), branch slashes→dashes and
/// lowercased (matching `sanitize_branch` on window names). The join's *both* sides must key
/// through this — the item side via [`window_key`], the tmux side after stripping the agent-window
/// glyph — so an item and its window agree.
pub fn window_key_parts(repo: &str, branch: &str) -> String {
    let r = repo.rsplit('/').next().unwrap_or(repo).to_lowercase();
    let b = branch.replace('/', "-").to_lowercase();
    format!("{r}:{b}")
}

/// The window key for an item, or `None` when it carries no locator (so it can have no window — it
/// resolves to launching/orphaned).
pub fn window_key(item: &AttnItem) -> Option<String> {
    match (&item.repo, &item.branch) {
        (Some(r), Some(b)) => Some(window_key_parts(r, b)),
        _ => None,
    }
}

/// Join in-flight items to their window snapshots and classify each — the input to the in-progress
/// / needs-you lanes. Pure: the caller supplies `windows` (keyed `repo:branch`, built from one
/// `list-windows -a`) and the idle/stuck thresholds.
pub fn classify_started(
    items: Vec<StartedItem>,
    windows: &std::collections::HashMap<String, WindowState>,
    now: i64,
    idle_mins: i64,
    stuck_mins: i64,
) -> Vec<(StartedItem, WorkStatus)> {
    items
        .into_iter()
        .map(|it| {
            let win = window_key(&it.item).and_then(|k| windows.get(&k));
            let status = work_status(
                it.done,
                it.since,
                win,
                now,
                idle_mins,
                stuck_mins,
                START_GRACE_SECS,
            );
            (it, status)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attention::*;
    use crate::test_support::with_state_dir;

    fn win(agent: &str, since: i64) -> WindowState {
        WindowState {
            agent: agent.into(),
            agent_since: since,
            ..Default::default()
        }
    }

    #[test]
    fn work_status_precedence_and_grace() {
        // done wins regardless of any window state
        assert_eq!(
            work_status(true, 0, Some(&win("working", 0)), 100, 5, 15, 180),
            WorkStatus::Done
        );
        // no window: launching within grace, orphaned once past it
        assert_eq!(
            work_status(false, 100, None, 100 + 180, 5, 15, 180),
            WorkStatus::Launching
        );
        assert_eq!(
            work_status(false, 100, None, 100 + 181, 5, 15, 180),
            WorkStatus::Orphaned
        );
        // real agent signals are trusted
        assert_eq!(
            work_status(false, 0, Some(&win("waiting", 0)), 100, 5, 15, 180),
            WorkStatus::Waiting
        );
        assert_eq!(
            work_status(false, 0, Some(&win("done", 0)), 100, 5, 15, 180),
            WorkStatus::Done
        );
    }

    #[test]
    fn work_status_ages_working_into_idle_and_stuck() {
        let now = 10_000;
        assert_eq!(
            work_status(false, 0, Some(&win("working", now - 60)), now, 5, 15, 180),
            WorkStatus::Working
        );
        assert_eq!(
            work_status(
                false,
                0,
                Some(&win("working", now - 6 * 60)),
                now,
                5,
                15,
                180
            ),
            WorkStatus::Idle
        );
        assert_eq!(
            work_status(
                false,
                0,
                Some(&win("working", now - 20 * 60)),
                now,
                5,
                15,
                180
            ),
            WorkStatus::Stuck
        );
        // unknown `since` can't be aged → working
        assert_eq!(
            work_status(false, 0, Some(&win("working", 0)), now, 5, 15, 180),
            WorkStatus::Working
        );
    }

    #[test]
    fn work_status_unknown_agent_falls_back_to_window_flags() {
        let flags = |done_flag, blockers| WindowState {
            agent: String::new(),
            agent_since: 0,
            done_flag,
            blockers,
        };
        assert_eq!(
            work_status(false, 0, Some(&flags(true, false)), 100, 5, 15, 180),
            WorkStatus::Done
        );
        assert_eq!(
            work_status(false, 0, Some(&flags(false, true)), 100, 5, 15, 180),
            WorkStatus::Blocked
        );
        assert_eq!(
            work_status(false, 0, Some(&flags(false, false)), 100, 5, 15, 180),
            WorkStatus::Working
        );
    }

    #[test]
    fn work_status_lane_mapping() {
        use WorkStatus::*;
        for s in [Done, Stuck, Waiting, Blocked] {
            assert_eq!(s.lane(), Lane::NeedsYou, "{}", s.label());
        }
        for s in [Working, Idle, Launching] {
            assert_eq!(s.lane(), Lane::InProgress, "{}", s.label());
        }
        assert_eq!(Orphaned.lane(), Lane::ToStart);
        assert!(Stuck.needs_you() && !Working.needs_you());
    }

    #[test]
    fn window_key_parts_canonicalizes_repo_and_branch() {
        // owner/name → name; slashes → dashes; lowercased — so an item and its tmux window agree.
        assert_eq!(window_key_parts("eduuh/Bn", "feature/X"), "bn:feature-x");
        assert_eq!(window_key_parts("bn", "feat-x"), "bn:feat-x");
    }

    #[test]
    fn classify_started_joins_window_by_repo_branch() {
        with_state_dir(|| {
            let _now = crate::test_support::EnvGuard::set("BN_NOW", "1000");
            let a = add_new(
                &NewAttn {
                    source: "manual",
                    title: "with window",
                    severity: "warn",
                    repo: Some("bn"),
                    branch: Some("feat-x"),
                    ..Default::default()
                },
                "2026-06-23",
            )
            .unwrap();
            let b = add_new(
                &NewAttn {
                    source: "manual",
                    title: "no window",
                    severity: "warn",
                    repo: Some("bn"),
                    branch: Some("feat-y"),
                    ..Default::default()
                },
                "2026-06-23",
            )
            .unwrap();
            mark_started(&a.id).unwrap();
            mark_started(&b.id).unwrap();

            let mut windows = std::collections::HashMap::new();
            windows.insert("bn:feat-x".to_string(), win("working", 1_000));
            // now=1200: a's window has been working 200s (< 5m) → Working; b has no window and is
            // 200s past its start (> 180s grace) → Orphaned.
            let out = classify_started(started_items(), &windows, 1_200, 5, 15);
            let by_id = |id: &str| {
                out.iter()
                    .find(|(s, _)| s.item.id == id)
                    .map(|(_, st)| *st)
                    .unwrap()
            };
            assert_eq!(by_id(&a.id), WorkStatus::Working);
            assert_eq!(by_id(&b.id), WorkStatus::Orphaned);
        });
    }

    #[test]
    fn lifecycle_metrics_counts_by_mode_and_measures_inflight() {
        with_state_dir(|| {
            let now = crate::test_support::EnvGuard::set("BN_NOW", "1000");
            add_simple("manual one", "warn", None); // manual
            add_new(
                &NewAttn {
                    source: "manual",
                    title: "assist one",
                    severity: "warn",
                    repo: Some("bn"),
                    branch: Some("b"),
                    mode: Some("assist"),
                    prompt: Some("p"),
                    ..Default::default()
                },
                "2026-06-23",
            )
            .unwrap();
            let auto = add_auto("auto one", "manual"); // auto
                                                       // in-flight 600s: started 1000 → done 1600 → acked 1700
            mark_started(&auto.id).unwrap();
            now.set_value("1600");
            mark_done(&auto.id, None, None).unwrap();
            now.set_value("1700");
            ack(&auto.id).unwrap();

            let m = lifecycle_metrics(2000, 30);
            assert_eq!(m.raised, 3);
            assert_eq!((m.raised_manual, m.raised_assist, m.raised_auto), (1, 1, 1));
            assert_eq!((m.started, m.done, m.acked), (1, 1, 1));
            assert_eq!(m.avg_inflight_secs, Some(600));
            assert_eq!(m.median_inflight_secs, Some(600));
        });
    }

    #[test]
    fn lifecycle_metrics_respects_the_window() {
        with_state_dir(|| {
            let now = crate::test_support::EnvGuard::set("BN_NOW", "1"); // ancient — outside the window
            add_simple("old", "warn", None);
            now.set_value("1000000"); // recent — inside
            add_simple("new", "warn", None);
            // now = 1_000_100, 1-day window → cutoff ~913_700: the ancient raise drops.
            let m = lifecycle_metrics(1_000_100, 1);
            assert_eq!(m.raised, 1);
        });
    }
}
