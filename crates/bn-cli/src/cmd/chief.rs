//! `bn chief` — the AM/PM chief-of-staff briefing + its cron jobs.
use crate::*;

/// Pick the brief slot when `bn chief` is run bare: mornings (before noon local) → AM, afternoons
/// and evenings → PM. Falls back to AM when the local clock can't be read.
pub(crate) fn default_chief_slot() -> bn_core::chief::Slot {
    match bn_core::night::local_minutes(&bn_core::clock::RealClock) {
        Some(m) if m >= 12 * 60 => bn_core::chief::Slot::Pm,
        _ => bn_core::chief::Slot::Am,
    }
}

/// The traceable source label for a `bn next` action's kind — what the AM card's top-3 points back
/// to ("review you/bn#12 (review)").
pub(crate) fn chief_action_source(kind: bn_core::next::ActionKind) -> String {
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
pub(crate) fn chief_night_items(hours: i64) -> Vec<bn_core::night::NightReportItem> {
    let cutoff = bn_core::util::now_epoch_secs() - hours.max(0) * 3600;
    bn_core::night::report_since(cutoff)
}

/// Quarter status for the card, or `None` when there's no current quarter file / no goals yet.
pub(crate) fn chief_quarter(cfg: &Config, today: &str) -> Option<bn_core::chief::QuarterStatus> {
    let qid = bn_core::quarter::quarter_id(today)?;
    let q = bn_core::quarter::read_quarter(cfg, &qid).ok().flatten()?;
    if q.front.goals.is_empty() {
        return None;
    }
    let elapsed = bn_core::quarter::elapsed_fraction(&qid, today).unwrap_or(0.0);
    Some(bn_core::chief::QuarterStatus {
        elapsed_pct: (elapsed * 100.0).round().clamp(0.0, 100.0) as u8,
        on_track: q.on_track_count(),
        total: q.front.goals.len(),
        at_risk: q.top_at_risk().map(|g| g.text.clone()),
        qid,
    })
}

/// The daily rollup the card summarizes: yesterday's for the AM brief, today's for the PM shutdown.
pub(crate) fn chief_daily(
    cfg: &Config,
    slot: bn_core::chief::Slot,
    today: &str,
) -> Option<bn_core::chief::DailySummary> {
    let d = match slot {
        bn_core::chief::Slot::Am => bn_core::daily::previous_day(cfg, today)?,
        bn_core::chief::Slot::Pm => {
            let path = cfg.daily_file(today);
            if !path.exists() {
                return None;
            }
            bn_core::daily::read(&path).ok()?
        }
    };
    Some(bn_core::chief::DailySummary {
        done: d.completed().count(),
        open: d.open().count(),
        done_titles: d.completed().map(|g| g.text.clone()).collect(),
        open_titles: d.open().map(|g| g.text.clone()).collect(),
    })
}

/// Assemble the Chief of Staff packet from the offline stores, degrading per-source: a source that
/// is missing or unreadable simply omits its section (never an error).
pub(crate) fn gather_chief_packet(
    cfg: &Config,
    slot: bn_core::chief::Slot,
    today: &str,
) -> bn_core::chief::Packet {
    use bn_core::chief;
    let now = bn_core::util::now_epoch_secs();
    let mut p = chief::Packet::new(today, slot);

    // Night shift (AM only): draft PRs waiting on review.
    if slot == chief::Slot::Am {
        p.night = chief_night_items(24)
            .into_iter()
            .filter(|it| it.outcome == bn_core::night::NightOutcome::PrReady)
            .filter_map(|it| {
                it.link.as_ref().map(|link| chief::NightPr {
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
        .map(|a| chief::TopAction {
            title: a.title.clone(),
            source: chief_action_source(a.kind),
            link: a.link.clone(),
        })
        .collect();

    // Calendar cache — the `meetings` feed source, best-effort.
    p.calendar = bn_core::feed::read_all()
        .into_iter()
        .find(|s| s.source == "meetings")
        .and_then(|s| {
            let texts: Vec<String> = s.items.into_iter().map(|i| i.text).collect();
            chief::calendar_shape(&texts)
        });

    // Quarter status.
    p.quarter = chief_quarter(cfg, today);

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
        p.inbox = Some(chief::InboxStatus {
            count: inbox_items.len(),
            oldest_age_days,
        });
    }

    // Daily rollup (yesterday's for AM, today's for PM).
    p.daily = chief_daily(cfg, slot, today);

    // Folded-in surfaces (#274): the morning brief absorbs the standup line and the evening
    // shutdown absorbs the wrap digest, retiring the standalone 09:00/17:00 push crons. Each is
    // gated by the same toggle that used to drive its cron ([standup].enabled / [wrap].enabled),
    // so opting into standup/wrap now folds it into the one card instead of a separate toast.
    let jc = resolver::JobConfig::load();

    // Standup line (AM) — offline only. The one-shot PR delta stays with the manual
    // `bn run standup-push`, keeping the brief's deterministic/offline contract intact.
    if slot == chief::Slot::Am && jc.standup_enabled {
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
    if slot == chief::Slot::Pm && jc.wrap_enabled {
        let note_since = target_ctx(cfg, None, None)
            .and_then(|ctx| bn_core::store::read(&cfg.note_file(&ctx)).ok())
            .map(|note| bn_core::reports::note_last_activity(&note))
            .unwrap_or_else(|| today.to_string());
        let w = bn_core::reports::wrap(cfg, today, &note_since, &bn_core::runner::RealRunner);
        let summary = chief::WrapSummary {
            unnoted_commits: w.unnoted_commits.len(),
            merged_prs: w
                .merged_prs
                .iter()
                .map(|pr| chief::WrapPr {
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

/// AM side-effect summary — delegates the idempotent daily writes to `chief::add_night_goals`.
pub(crate) fn chief_am_effect(
    cfg: &Config,
    today: &str,
    night: &[bn_core::chief::NightPr],
) -> String {
    let added = bn_core::chief::add_night_goals(cfg, today, night);
    if night.is_empty() {
        "no night PRs to review".to_string()
    } else if added == 0 {
        "review goals already queued".to_string()
    } else {
        format!("added {added} review goal(s) to today's daily")
    }
}

/// PM side-effect summary — delegates the idempotent scheduling to `chief::schedule_tomorrow`.
pub(crate) fn chief_pm_effect(
    cfg: &Config,
    today: &str,
    packet: &bn_core::chief::Packet,
) -> String {
    use bn_core::chief::TomorrowOutcome;
    match bn_core::chief::schedule_tomorrow(cfg, today, packet) {
        TomorrowOutcome::Scheduled { text, .. } => {
            format!("scheduled tomorrow's first task: {text}")
        }
        TomorrowOutcome::AlreadySet { text } => {
            format!("tomorrow's first task already set: {text}")
        }
        TomorrowOutcome::Empty => "nothing to carry — clean slate tomorrow".to_string(),
        TomorrowOutcome::Failed(e) => format!("couldn't schedule tomorrow's task: {e}"),
    }
}

/// Map a card to feed items (headline first, then each structured item) so `bn feed` surfaces it
/// under the `chief-am` / `chief-pm` source. A run replaces the source, so yesterday's card falls
/// off on its own.
pub(crate) fn chief_feed_items(card: &bn_core::chief::Card) -> Vec<bn_core::feed::FeedItem> {
    let mut items = vec![bn_core::feed::FeedItem {
        key: format!("{}-headline", card.agent),
        text: card.headline.clone(),
        url: String::new(),
    }];
    for (i, it) in card.items.iter().enumerate() {
        items.push(bn_core::feed::FeedItem {
            key: format!("{}-{i}", card.agent),
            text: it.title.clone(),
            url: it.link.clone().unwrap_or_default(),
        });
    }
    items
}

/// The Chief of Staff pipeline shared by `bn chief` and the `staff-chief-{am,pm}` crons: gather →
/// build the card → apply the card's side-effects (AM: a review goal per night PR; PM: tomorrow's
/// first task) → mirror the card to the feed. Returns the card and a one-line summary of what it
/// changed.
pub(crate) fn run_chief(
    cfg: &Config,
    slot: bn_core::chief::Slot,
    today: &str,
) -> (bn_core::chief::Card, String) {
    use bn_core::chief::Slot;
    let packet = gather_chief_packet(cfg, slot, today);
    let card = bn_core::chief::build_card(&packet);
    let effect = match slot {
        Slot::Am => chief_am_effect(cfg, today, &packet.night),
        Slot::Pm => chief_pm_effect(cfg, today, &packet),
    };
    let _ = bn_core::feed::ingest(slot.agent(), chief_feed_items(&card));
    (card, effect)
}

/// `bn chief [am|pm]` — run the Chief of Staff for a slot and print its card. Always runs (unlike
/// the crons, which gate on `[chief].enabled`). `--json` emits the full card envelope.
pub(crate) fn emit_chief(cfg: &Config, slot: bn_core::chief::Slot, as_json: bool) {
    let today = today();
    let (card, effect) = run_chief(cfg, slot, &today);
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "chief",
                serde_json::json!({ "card": card, "effect": effect })
            )
            .to_json()
        );
        return;
    }
    print!("{}", bn_core::chief::render(&card));
    println!("\n— {effect}");
}

/// `staff-chief-am` cron — the 05:30 morning brief. Opt-in via `[chief].enabled`.
pub(crate) fn job_staff_chief_am(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    run_chief_job(cfg, bn_core::chief::Slot::Am)
}

/// `staff-chief-pm` cron — the 16:45 evening shutdown. Opt-in via `[chief].enabled`.
pub(crate) fn job_staff_chief_pm(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    run_chief_job(cfg, bn_core::chief::Slot::Pm)
}

/// Shared cron entry for both Chief of Staff slots: gate on `[chief].enabled` (a clean no-op when
/// off), else run the pipeline and print a compact summary. Always exits 0 — a declined gate isn't
/// a failure.
pub(crate) fn run_chief_job(cfg: &Config, slot: bn_core::chief::Slot) -> i32 {
    if !resolver::JobConfig::load().chief_enabled {
        println!(
            "{}: Chief of Staff disabled — set [chief].enabled to opt in",
            slot.agent()
        );
        return 0;
    }
    let (card, effect) = run_chief(cfg, slot, &today());
    println!("{}: {}", slot.agent(), card.headline);
    println!("  {effect}");
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use bn_core::chief::Slot;
    use bn_core::test_support::{config_with_hostname, with_state_and_notes_dir, EnvGuard};

    const TODAY: &str = "2026-07-06";

    // #274: the AM brief folds in the standup line, gated by [standup].enabled and a signal check.
    #[test]
    fn am_gather_folds_standup_when_enabled_with_signal() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        with_state_and_notes_dir(&root, "testhost", || {
            let cfg = config_with_hostname(&root, "testhost");
            bn_core::daily::add(&cfg, TODAY, "write the docs", None, None).unwrap();
            let _on = EnvGuard::set("BN_STANDUP_ENABLED", "1");
            let p = gather_chief_packet(&cfg, Slot::Am, TODAY);
            assert!(
                p.standup
                    .as_deref()
                    .unwrap_or("")
                    .contains("today 1 planned"),
                "{:?}",
                p.standup
            );
        });
    }

    #[test]
    fn am_gather_skips_standup_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        with_state_and_notes_dir(&root, "testhost", || {
            let cfg = config_with_hostname(&root, "testhost");
            bn_core::daily::add(&cfg, TODAY, "write the docs", None, None).unwrap();
            let _off = EnvGuard::set("BN_STANDUP_ENABLED", "0");
            let p = gather_chief_packet(&cfg, Slot::Am, TODAY);
            assert!(p.standup.is_none());
        });
    }

    #[test]
    fn am_gather_skips_standup_without_signal() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        with_state_and_notes_dir(&root, "testhost", || {
            let cfg = config_with_hostname(&root, "testhost");
            // Empty day → no signal even with the toggle on, so the brief stays quiet.
            let _on = EnvGuard::set("BN_STANDUP_ENABLED", "1");
            let p = gather_chief_packet(&cfg, Slot::Am, TODAY);
            assert!(p.standup.is_none(), "{:?}", p.standup);
        });
    }

    // #274: the PM shutdown folds in the wrap, gated by [wrap].enabled. When off, the gather
    // short-circuits before touching git — proven here by the clean, offline no-op.
    #[test]
    fn pm_gather_skips_wrap_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        with_state_and_notes_dir(&root, "testhost", || {
            let cfg = config_with_hostname(&root, "testhost");
            let _off = EnvGuard::set("BN_WRAP_ENABLED", "0");
            let p = gather_chief_packet(&cfg, Slot::Pm, TODAY);
            assert!(p.wrap.is_none());
        });
    }
}
