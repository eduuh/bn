//! `bn archivist` — the Friday private weekly chronicle + its `staff-archivist` cron job.
//!
//! The pure `Packet → Chronicle → markdown` core lives in [`bn_core::archivist`]; this is the IO
//! shell that gathers the packet from the live offline stores, validates the rendered chronicle
//! *before* it lands, writes it into the private notes repo, and mirrors any proposed brag entries
//! to the feed (as card items only — never auto-logged). Mirrors the Chief-of-Staff pipeline
//! ([`super::chief`]): deterministic, offline reads only.
use crate::*;

/// The objective metrics delta for the window, lifted from the weekly [`bn_core::insights`] retro
/// so the numbers agree with `bn insights` (offline; no network).
pub(crate) fn gather_metrics(
    ins: &bn_core::insights::Insights,
) -> bn_core::archivist::MetricsDelta {
    bn_core::archivist::MetricsDelta {
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
) -> Option<bn_core::archivist::QuarterStatus> {
    let qid = bn_core::quarter::quarter_id(today)?;
    let q = bn_core::quarter::read_quarter(cfg, &qid).ok().flatten()?;
    if q.front.goals.is_empty() {
        return None;
    }
    Some(bn_core::archivist::QuarterStatus {
        on_track: q.on_track_count(),
        total: q.front.goals.len(),
        at_risk: q.top_at_risk().map(|g| g.text.clone()),
        qid,
    })
}

/// Staff activity — the current feed snapshot, one entry per non-empty source. Best-effort: the
/// feed keeps only the latest snapshot per source, so this reflects the staff's record as it
/// stands now, not a true weekly history.
pub(crate) fn gather_activity() -> Vec<bn_core::archivist::AgentActivity> {
    bn_core::feed::read_all()
        .into_iter()
        .filter(|s| !s.items.is_empty())
        .map(|s| bn_core::archivist::AgentActivity {
            agent: s.source,
            cards: s.items.len(),
        })
        .collect()
}

/// Completed daily goals across `[start..=end]` — the week's candidate wins (raw material for both
/// brag-proposal extraction and the "For the record" section).
pub(crate) fn gather_wins(cfg: &Config, start: &str, end: &str) -> Vec<bn_core::archivist::Win> {
    let mut out = Vec::new();
    for d in bn_core::daily::read_window(cfg, start, end) {
        for g in d.completed() {
            out.push(bn_core::archivist::Win {
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
) -> Vec<bn_core::archivist::LoggedItem> {
    let mut out: Vec<bn_core::archivist::LoggedItem> = bn_core::rollups::achievements(cfg)
        .into_iter()
        .filter(|a| a.date.as_str() >= start && a.date.as_str() <= end)
        .map(|a| bn_core::archivist::LoggedItem {
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
pub(crate) fn gather_archivist_packet(
    cfg: &Config,
    today: &str,
) -> Result<bn_core::archivist::Packet, String> {
    let Some(week) = bn_core::util::iso_week(today) else {
        return Err(format!("can't derive the ISO week for {today}"));
    };
    let ins = bn_core::insights::insights(cfg, today, None);
    let mut p = bn_core::archivist::Packet::new(week, ins.start.clone(), ins.end.clone());
    p.activity = gather_activity();
    p.metrics = Some(gather_metrics(&ins));
    p.achievements = gather_achievements(cfg, &ins.start, &ins.end);
    p.wins = gather_wins(cfg, &ins.start, &ins.end);
    p.quarter = gather_quarter(cfg, today);
    // interview: the Interviewer (#264) fills this when it lands; absent today, degrades cleanly.
    Ok(p)
}

/// Map the brag proposals to feed items (a headline, then one per proposal) so `bn feed` surfaces
/// them under the `archivist` source — as *card items only*. A run replaces the source, so last
/// week's proposals fall off on their own. These are drafts: `bn achieve` stays a human act.
pub(crate) fn archivist_feed_items(
    week: &str,
    proposals: &[bn_core::archivist::BragProposal],
) -> Vec<bn_core::feed::FeedItem> {
    let headline = if proposals.is_empty() {
        format!("{week}: chronicle written — no unlogged wins to propose")
    } else {
        format!(
            "{week}: {} draft brag entr{} to review — `bn achieve` (never auto-logged)",
            proposals.len(),
            if proposals.len() == 1 { "y" } else { "ies" }
        )
    };
    let mut items = vec![bn_core::feed::FeedItem {
        key: "archivist-headline".to_string(),
        text: headline,
        url: String::new(),
    }];
    for (i, pr) in proposals.iter().enumerate() {
        items.push(bn_core::feed::FeedItem {
            key: format!("archivist-{i}"),
            text: format!("draft brag: {}", pr.text),
            url: pr.link.clone().unwrap_or_default(),
        });
    }
    items
}

/// The result of a run — the built chronicle, where it landed, and how many brag entries it drafted.
pub(crate) struct ArchivistRun {
    pub chronicle: bn_core::archivist::Chronicle,
    pub path: std::path::PathBuf,
}

/// The Archivist pipeline shared by `bn archivist` and the `staff-archivist` cron: gather → build →
/// validate the rendered chronicle (refuse to land an invalid one) → write it into the private
/// notes repo → mirror any brag proposals to the feed. Returns the chronicle and its path.
pub(crate) fn run_archivist(cfg: &Config, today: &str) -> Result<ArchivistRun, String> {
    let packet = gather_archivist_packet(cfg, today)?;
    let chronicle = bn_core::archivist::build_chronicle(&packet);
    let md = bn_core::archivist::render(&chronicle);
    // Validated before landing — an invalid chronicle is never written.
    bn_core::archivist::validate(&md, chronicle.thin)?;

    let path = cfg.chronicle_file(&chronicle.week);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, &md).map_err(|e| e.to_string())?;

    // Brag proposals go to the feed as card items only — never auto-logged. A feed hiccup must not
    // fail the run: the chronicle (the real output) already landed.
    let items = archivist_feed_items(&chronicle.week, &chronicle.proposals);
    bn_core::util::best_effort(
        bn_core::feed::ingest("archivist", items),
        "archivist: feed ingest",
    );

    Ok(ArchivistRun { chronicle, path })
}

/// A one-line summary of the proposals a run drafted (or that it drafted none).
fn proposals_line(n: usize) -> String {
    if n == 0 {
        "no unlogged wins to propose".to_string()
    } else {
        format!(
            "{n} draft brag entr{} proposed (review + `bn achieve`; never auto-logged)",
            if n == 1 { "y" } else { "ies" }
        )
    }
}

/// `bn archivist` — write this week's private chronicle and print it. Always runs (unlike the
/// cron, which gates on `[archivist].enabled`). `--json` emits the full chronicle envelope + path.
pub(crate) fn emit_archivist(cfg: &Config, as_json: bool) {
    let today = today();
    match run_archivist(cfg, &today) {
        Ok(run) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::success(
                        "archivist",
                        serde_json::json!({
                            "chronicle": run.chronicle,
                            "path": run.path.display().to_string(),
                        })
                    )
                    .to_json()
                );
                return;
            }
            print!("{}", bn_core::archivist::render(&run.chronicle));
            println!("\n— written to {}", run.path.display());
            println!("— {}", proposals_line(run.chronicle.proposals.len()));
        }
        Err(e) => fail(&format!("archivist: {e}")),
    }
}

/// `staff-archivist` cron — the Friday 17:30 weekly chronicle. Opt-in via `[archivist].enabled` (a
/// clean no-op when off — the job writes into the notes repo). Exits 0 on a declined gate (not a
/// failure); non-zero only when the run itself fails.
pub(crate) fn job_staff_archivist(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    if !resolver::JobConfig::load().archivist_enabled {
        println!("staff-archivist: Archivist disabled — set [archivist].enabled to opt in");
        return 0;
    }
    match run_archivist(cfg, &today()) {
        Ok(run) => {
            println!("staff-archivist: chronicle written: {}", run.path.display());
            println!("  {}", proposals_line(run.chronicle.proposals.len()));
            0
        }
        Err(e) => {
            eprintln!("staff-archivist: {e}");
            1
        }
    }
}


