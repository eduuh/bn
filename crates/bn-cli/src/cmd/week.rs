//! `bn week gather` — the week's raw material, printed as JSON.
//!
//! The I/O edge over [`bn_core::week`]: read the staff feed, the metrics delta, the logged
//! achievements, the completed goals, the retro summary and the quarter — each degrading on its
//! own — and print. It writes nothing; the `staff-archivist` agent turns it into the week's story.
//!
//! The pure `Packet → Chronicle → markdown` core lives in [`bn_core::week`]; this is the IO
//! shell that gathers the packet from the live offline stores, validates the rendered chronicle
//! *before* it lands, writes it into the private notes repo, and mirrors any proposed brag entries
//! to the feed (as card items only — never auto-logged). Mirrors the Chief-of-Staff pipeline
//! ([`super::chief`]): deterministic, offline reads only.
use crate::*;

/// The objective metrics delta for the window, lifted from the weekly [`bn_core::insights`] retro
/// so the numbers agree with `bn insights` (offline; no network).
pub(crate) fn gather_metrics(
    ins: &bn_core::insights::Insights,
) -> bn_core::week::MetricsDelta {
    bn_core::week::MetricsDelta {
        merged: ins.pr_cycle.merged,
        prev_merged: ins.pr_cycle.prev_merged,
        cycle_median_hours: ins.pr_cycle.median_hours,
        prev_cycle_median_hours: ins.pr_cycle.prev_median_hours,
        goals_completed: ins.goal_completion.completed,
        goals_carried: ins.goal_completion.carried,
    }
}

/// Quarter status snapshot for the chronicle, or `None` when there's no current quarter file / no
/// goals yet. A snapshot, not a per-week diff — the quarter file carries no history to diff.
pub(crate) fn gather_quarter(
    cfg: &Config,
    today: &str,
) -> Option<bn_core::week::QuarterStatus> {
    let qid = bn_core::quarter::quarter_id(today)?;
    let q = bn_core::quarter::read_quarter(cfg, &qid).ok().flatten()?;
    if q.front.goals.is_empty() {
        return None;
    }
    Some(bn_core::week::QuarterStatus {
        on_track: q.on_track_count(),
        total: q.front.goals.len(),
        at_risk: q.top_at_risk().map(|g| g.text.clone()),
        qid,
    })
}

/// Staff activity — the current feed snapshot, one entry per non-empty source. Best-effort: the
/// feed keeps only the latest snapshot per source, so this reflects the staff's record as it
/// stands now, not a true weekly history.
pub(crate) fn gather_activity() -> Vec<bn_core::week::AgentActivity> {
    bn_core::feed::read_all()
        .into_iter()
        .filter(|s| !s.items.is_empty())
        .map(|s| bn_core::week::AgentActivity {
            agent: s.source,
            cards: s.items.len(),
        })
        .collect()
}

/// Completed daily goals across `[start..=end]` — the week's candidate wins (raw material for both
/// brag-proposal extraction and the "For the record" section).
pub(crate) fn gather_wins(cfg: &Config, start: &str, end: &str) -> Vec<bn_core::week::Win> {
    let mut out = Vec::new();
    for d in bn_core::daily::read_window(cfg, start, end) {
        for g in d.completed() {
            out.push(bn_core::week::Win {
                date: d.front.date.clone(),
                text: g.text.clone(),
                link: g.link.clone(),
            });
        }
    }
    out
}

/// Achievements logged within `[start..=end]`, chronological (newest last) — the "What changed"
/// source. Reads every note's `achievements.md` via [`bn_core::rollups::achievements`] (offline).
pub(crate) fn gather_achievements(
    cfg: &Config,
    start: &str,
    end: &str,
) -> Vec<bn_core::week::LoggedItem> {
    let mut out: Vec<bn_core::week::LoggedItem> = bn_core::rollups::achievements(cfg)
        .into_iter()
        .filter(|a| a.date.as_str() >= start && a.date.as_str() <= end)
        .map(|a| bn_core::week::LoggedItem {
            date: a.date,
            text: a.text,
        })
        .collect();
    out.sort_by(|a, b| a.date.cmp(&b.date));
    out
}

/// Assemble the Archivist packet from the offline stores, degrading per-source: a source that is
/// missing or unreadable simply omits its contribution (never an error). Errors only when the ISO
/// week can't be derived from `today`.
pub(crate) fn gather_week_packet(
    cfg: &Config,
    today: &str,
) -> Result<bn_core::week::Packet, String> {
    let Some(week) = bn_core::util::iso_week(today) else {
        return Err(format!("can't derive the ISO week for {today}"));
    };
    let ins = bn_core::insights::insights(cfg, today, None);
    let mut p = bn_core::week::Packet::new(week, ins.start.clone(), ins.end.clone());
    p.activity = gather_activity();
    p.metrics = Some(gather_metrics(&ins));
    p.achievements = gather_achievements(cfg, &ins.start, &ins.end);
    p.wins = gather_wins(cfg, &ins.start, &ins.end);
    p.quarter = gather_quarter(cfg, today);
    // interview: the Interviewer (#264) fills this when it lands; absent today, degrades cleanly.
    Ok(p.seal())
}

/// `bn week gather` — print the packet as JSON. A week that could not be dated is the one hard
/// failure: everything else degrades to an absent section.
pub(crate) fn emit_week_gather(cfg: &Config) -> i32 {
    match gather_week_packet(cfg, &today()) {
        Ok(packet) => {
            println!(
                "{}",
                serde_json::to_string(&packet).unwrap_or_else(|_| "{}".into())
            );
            0
        }
        Err(e) => {
            eprintln!("bn week gather: {e}");
            1
        }
    }
}
