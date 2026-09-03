//! `bn briefing gather` — the day's state folded from every store, printed as JSON.
//!
//! The I/O edge over [`bn_core::briefing`]: read the night shift, the ranked actions, the calendar
//! cache, the quarter, the `q` inbox, the daily rollup, the standup line and the wrap — each
//! degrading on its own — and print. It writes nothing and decides nothing; the
//! `staff-chief-{am,pm}` agents own what the day means.
use crate::*;

/// Pick the brief slot when `bn chief` is run bare: mornings (before noon local) → AM, afternoons
/// and evenings → PM. Falls back to AM when the local clock can't be read.
pub(crate) fn default_slot() -> bn_core::briefing::Slot {
    match bn_core::night::local_minutes(&bn_core::clock::RealClock) {
        Some(m) if m >= 12 * 60 => bn_core::briefing::Slot::Pm,
        _ => bn_core::briefing::Slot::Am,
    }
}

/// The traceable source label for a `bn next` action's kind — what the AM card's top-3 points back
/// to ("review you/bn#12 (review)").
pub(crate) fn action_source(kind: bn_core::next::ActionKind) -> String {
    use bn_core::next::ActionKind;
    match kind {
        ActionKind::Wip => "wip",
        ActionKind::Cron => "cron",
        ActionKind::Attn => "attn",
        ActionKind::Meeting => "meeting",
        ActionKind::Email => "email",
        ActionKind::Mention => "teams",
        ActionKind::Review => "review",
        ActionKind::Pr => "pr",
        ActionKind::Goal => "goal",
        ActionKind::Blocked => "blocked",
    }
    .to_string()
}

/// The night shift's dispatched items over the last `hours`, classified by outcome — the same read
/// `bn night report` does, reused for the AM card's review queue. Delegates to the shared
/// [`bn_core::night::report_since`] so the card and the `night-shift` findings gatherer stay in
/// lockstep; `hours` is clamped to a non-negative cutoff exactly as before.
pub(crate) fn night_items(hours: i64) -> Vec<bn_core::night::NightReportItem> {
    let cutoff = bn_core::util::now_epoch_secs() - hours.max(0) * 3600;
    bn_core::night::report_since(cutoff)
}

/// Quarter status for the card, or `None` when there's no current quarter file / no goals yet.
pub(crate) fn quarter_status(cfg: &Config, today: &str) -> Option<bn_core::briefing::QuarterStatus> {
    let qid = bn_core::quarter::quarter_id(today)?;
    let q = bn_core::quarter::read_quarter(cfg, &qid).ok().flatten()?;
    if q.front.goals.is_empty() {
        return None;
    }
    let elapsed = bn_core::quarter::elapsed_fraction(&qid, today).unwrap_or(0.0);
    Some(bn_core::briefing::QuarterStatus {
        elapsed_pct: (elapsed * 100.0).round().clamp(0.0, 100.0) as u8,
        on_track: q.on_track_count(),
        total: q.front.goals.len(),
        at_risk: q.top_at_risk().map(|g| g.text.clone()),
        qid,
    })
}

/// The daily rollup the card summarizes: yesterday's for the AM brief, today's for the PM shutdown.
pub(crate) fn daily_summary(
    cfg: &Config,
    slot: bn_core::briefing::Slot,
    today: &str,
) -> Option<bn_core::briefing::DailySummary> {
    let d = match slot {
        bn_core::briefing::Slot::Am => bn_core::daily::previous_day(cfg, today)?,
        bn_core::briefing::Slot::Pm => {
            let path = cfg.daily_file(today);
            if !path.exists() {
                return None;
            }
            bn_core::daily::read(&path).ok()?
        }
    };
    Some(bn_core::briefing::DailySummary {
        done: d.completed().count(),
        open: d.open().count(),
        done_titles: d.completed().map(|g| g.text.clone()).collect(),
        open_titles: d.open().map(|g| g.text.clone()).collect(),
    })
}

/// Assemble the Chief of Staff packet from the offline stores, degrading per-source: a source that
/// is missing or unreadable simply omits its section (never an error).
pub(crate) fn gather_briefing_packet(
    cfg: &Config,
    slot: bn_core::briefing::Slot,
    today: &str,
) -> bn_core::briefing::Packet {
    use bn_core::briefing;
    let now = bn_core::util::now_epoch_secs();
    let mut p = briefing::Packet::new(today, slot);

    // Night shift (AM only): draft PRs waiting on review.
    if slot == briefing::Slot::Am {
        p.night = night_items(24)
            .into_iter()
            .filter(|it| it.outcome == bn_core::night::NightOutcome::PrReady)
            .filter_map(|it| {
                it.link.as_ref().map(|link| briefing::NightPr {
                    slug: bn_core::night::pr_slug(&it.repo, link),
                    repo: it.repo.clone(),
                    link: link.clone(),
                })
            })
            .collect();
    }

    // `bn next` top actions + momentum context (all offline reads).
    let actions = bn_core::next::apply_momentum(
        cfg,
        bn_core::next::next_actions(cfg, today, now),
        today,
        now,
    );
    p.momentum =
        actions
            .iter()
            .find(|a| a.momentum.is_some())
            .map(|a| match (&a.repo, &a.branch) {
                (Some(r), Some(b)) => format!("{r}/{b}"),
                (Some(r), None) => r.clone(),
                _ => a.title.clone(),
            });
    p.actions = actions
        .iter()
        .take(3)
        .map(|a| briefing::TopAction {
            title: a.title.clone(),
            source: action_source(a.kind),
            link: a.link.clone(),
        })
        .collect();

    // Calendar cache — the `meetings` feed source, best-effort.
    p.calendar = bn_core::feed::read_all()
        .into_iter()
        .find(|s| s.source == "meetings")
        .and_then(|s| {
            let texts: Vec<String> = s.items.into_iter().map(|i| i.text).collect();
            briefing::calendar_shape(&texts)
        });

    // Quarter status.
    p.quarter = quarter_status(cfg, today);

    // `q` inbox — count + oldest item age.
    let inbox_items = bn_core::inbox::read(cfg);
    if !inbox_items.is_empty() {
        let oldest = inbox_items
            .iter()
            .filter_map(|i| bn_core::util::epoch_day(&i.created))
            .min();
        let oldest_age_days = match (oldest, bn_core::util::epoch_day(today)) {
            (Some(o), Some(t)) => Some((t - o).max(0)),
            _ => None,
        };
        p.inbox = Some(briefing::InboxStatus {
            count: inbox_items.len(),
            oldest_age_days,
        });
    }

    // Daily rollup (yesterday's for AM, today's for PM).
    p.daily = daily_summary(cfg, slot, today);

    // Folded-in surfaces (#274): the morning brief absorbs the standup line and the evening
    // shutdown absorbs the wrap digest, retiring the standalone 09:00/17:00 push crons. Each is
    // gated by the same toggle that used to drive its cron ([standup].enabled / [wrap].enabled),
    // so opting into standup/wrap now folds it into the one card instead of a separate toast.
    let jc = resolver::JobConfig::load();

    // Standup line (AM) — offline only. The one-shot PR delta stays with the manual
    // `bn run standup-push`, keeping the brief's deterministic/offline contract intact.
    if slot == briefing::Slot::Am && jc.standup_enabled {
        let s = bn_core::reports::standup(cfg, today);
        let has_signal = s
            .yesterday
            .as_ref()
            .map(|y| !y.completed.is_empty())
            .unwrap_or(false)
            || !s.today.is_empty()
            || !s.blockers.is_empty()
            || s.quarter_at_risk.is_some();
        if has_signal {
            p.standup = Some(bn_core::reports::standup_line(&s));
        }
    }

    // Wrap summary (PM) — compact: only the tails the shutdown card didn't already carry.
    if slot == briefing::Slot::Pm && jc.wrap_enabled {
        let note_since = target_ctx(cfg, None, None)
            .and_then(|ctx| bn_core::store::read(&cfg.note_file(&ctx)).ok())
            .map(|note| bn_core::reports::note_last_activity(&note))
            .unwrap_or_else(|| today.to_string());
        let w = bn_core::reports::wrap(cfg, today, &note_since, &bn_core::runner::RealRunner);
        let summary = briefing::WrapSummary {
            unnoted_commits: w.unnoted_commits.len(),
            merged_prs: w
                .merged_prs
                .iter()
                .map(|pr| briefing::WrapPr {
                    label: format!("#{} {}", pr.number, pr.title),
                    url: pr.url.clone(),
                })
                .collect(),
        };
        if !summary.is_empty() {
            p.wrap = Some(summary);
        }
    }

    p
}

/// `bn briefing gather --slot <am|pm>` — print the packet as JSON. Always exits 0: a quiet day is
/// a legitimately thin packet, and the reader's job is to say so rather than to invent a brief.
pub(crate) fn emit_briefing_gather(cfg: &Config, slot: bn_core::briefing::Slot) -> i32 {
    let packet = gather_briefing_packet(cfg, slot, &today());
    println!(
        "{}",
        serde_json::to_string(&packet).unwrap_or_else(|_| "{}".into())
    );
    0
}
