//! `bn quarter` — quarterly objectives + impact review.
use crate::*;

/// `bn quarter [YYYY-Qn]` / `add` / `done` / `edit` / `review` (#252). The bare view auto-creates
/// the current quarter (goals + elapsed-vs-progress header); a past `YYYY-Qn` is read-only.
pub(crate) fn emit_quarter(
    cfg: &Config,
    action: Option<QuarterCmd>,
    qid: Option<String>,
    as_json: bool,
) {
    use bn_core::quarter;
    match action {
        None => {
            let today = today();
            let (view_qid, q) = match qid {
                // A specific quarter — read-only.
                Some(id) => match quarter::read_quarter(cfg, &id) {
                    Ok(Some(q)) => (id, q),
                    Ok(None) => {
                        if as_json {
                            println!(
                                "{}",
                                Envelope::success(
                                    "quarter",
                                    serde_json::json!({ "quarter": id, "goals": [], "open": 0, "done": 0, "on_track": 0, "total": 0, "reviews": [] })
                                )
                                .to_json()
                            );
                        } else {
                            println!("No quarter goals for {id}");
                        }
                        return;
                    }
                    Err(e) => {
                        emit_err("quarter", &e, as_json);
                        return;
                    }
                },
                // Current quarter — auto-create.
                None => {
                    let q = quarter::ensure_current(cfg, &today)
                        .unwrap_or_else(|e| fail(&format!("bn quarter: {e}")));
                    (q.front.quarter.clone(), q)
                }
            };
            let _ = view_qid;
            if as_json {
                println!(
                    "{}",
                    Envelope::success("quarter", payload::quarter_data(&q, &today)).to_json()
                );
            } else {
                print!("{}", quarter::render(&q, &today));
            }
        }
        Some(QuarterCmd::Add { text, key, metric }) => {
            match quarter::add(
                cfg,
                &today(),
                &text.join(" "),
                key.as_deref(),
                metric.as_deref(),
            ) {
                Ok(a) => {
                    if as_json {
                        println!(
                            "{}",
                            Envelope::success("quarter_add", payload::quarter_add_data(&a))
                                .to_json()
                        );
                    } else {
                        println!("Added to {} [{}]: {}", a.quarter, a.key, a.text);
                    }
                }
                Err(e) => emit_err("quarter_add", &e, as_json),
            }
        }
        Some(QuarterCmd::Done { query }) => match quarter::done(cfg, &today(), &query.join(" ")) {
            Ok(d) => {
                if as_json {
                    println!(
                        "{}",
                        Envelope::success("quarter_done", payload::quarter_done_data(&d)).to_json()
                    );
                } else {
                    println!("Done [{}]: {}", d.key, d.text);
                }
            }
            Err(e) => emit_err("quarter_done", &e, as_json),
        },
        Some(QuarterCmd::Edit) => {
            let today = today();
            let q = quarter::ensure_current(cfg, &today)
                .unwrap_or_else(|e| fail(&format!("bn quarter: {e}")));
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nvim".into());
            let status = std::process::Command::new(editor)
                .arg(cfg.quarter_file(&q.front.quarter))
                .status();
            std::process::exit(status.ok().and_then(|s| s.code()).unwrap_or(1));
        }
        Some(QuarterCmd::Review { agent, harness }) => {
            emit_quarter_review(cfg, &agent, harness, as_json)
        }
        Some(QuarterCmd::Advise { capacity }) => emit_quarter_advise(cfg, capacity, as_json),
    }
}

/// `bn quarter review` — run the impact agent (also the `quarter-review` cron entry point). The
/// opt-in gate is applied once by `bn run`, from the manifest, so this always proceeds.
pub(crate) fn emit_quarter_review(
    cfg: &Config,
    agent: &str,
    harness: Option<String>,
    as_json: bool,
) {
    use bn_core::quarter;
    let jc = resolver::JobConfig::load();
    let harness = harness
        .or_else(|| jc.quarter_harness.clone())
        .or_else(|| cfg.review_harness.clone());
    let runner = bn_core::runner::RealRunner;
    let today = today();
    match quarter::review(
        cfg,
        &runner,
        agent,
        harness.as_deref(),
        &today,
        jc.quarter_review_window_days,
    ) {
        Ok(outcome) => {
            let qid = quarter::quarter_id(&today).unwrap_or_default();
            if as_json {
                println!(
                    "{}",
                    Envelope::success(
                        "quarter_review",
                        payload::quarter_review_data(&outcome, &qid)
                    )
                    .to_json()
                );
            } else {
                match &outcome {
                    quarter::ReviewOutcome::Appended { review, raised } => {
                        println!(
                            "Impact review appended to {qid} ({} goals assessed, {raised} flagged)",
                            review.assessments.len()
                        );
                        println!("  Recommendation: {}", review.recommendation);
                    }
                    quarter::ReviewOutcome::Malformed { reason } => {
                        println!(
                            "Impact review NOT recorded — agent output failed validation: {reason}"
                        );
                        println!("  A warn attention item was raised.");
                    }
                    quarter::ReviewOutcome::Skipped { reason } => {
                        println!("Quarter review skipped: {reason}");
                    }
                }
            }
        }
        Err(e) => fail(&format!("bn quarter review: {e}")),
    }
}

/// `bn quarter advise` — rank the pending backlog into a goal-aligned shortlist (#458). The
/// strategic layer over `bn next`: the pure [`bn_core::advise::advise`] ranker weighs each pending
/// candidate by which quarter goal it advances (slipping goals first), lifted by open attention on
/// its repo, capped to `capacity` slots. `--json` is the shape the orchestrator consumes.
pub(crate) fn emit_quarter_advise(cfg: &Config, capacity: usize, as_json: bool) {
    use bn_core::advise::{self, AdviseInputs, AttnSignal, Candidate, GoalContext};
    use bn_core::{backlog, quarter};
    let today = today();
    let q = quarter::ensure_current(cfg, &today)
        .unwrap_or_else(|e| fail(&format!("bn quarter advise: {e}")));
    let qid = q.front.quarter.clone();

    let goals: Vec<GoalContext> = q
        .open()
        .map(|g| GoalContext {
            key: g.key.clone(),
            text: g.text.clone(),
            status: q.status_of(&g.key).to_string(),
        })
        .collect();

    let bl = backlog::load(&backlog::backlog_path());
    let candidates: Vec<Candidate> = bl
        .entries()
        .iter()
        .filter(|e| e.status == backlog::BacklogStatus::Pending)
        .map(|e| Candidate {
            id: e.id.clone(),
            repo: e.repo.clone(),
            title: e.item.clone(),
            kind: e.kind,
            priority: e.priority,
            goal: None,
            band: None,
            added_at: e.added_at,
            not_after: e.not_after,
            est_secs: e.est_secs,
        })
        .collect();

    let attention: Vec<AttnSignal> = bn_core::attention::open_items(&today)
        .into_iter()
        .map(|a| AttnSignal {
            repo: a.repo,
            severity: a.severity,
        })
        .collect();

    let advice = advise::advise(&AdviseInputs {
        goals: &goals,
        candidates: &candidates,
        attention: &attention,
        capacity,
        now: bn_core::util::now_epoch_secs(),
    });

    if as_json {
        println!(
            "{}",
            Envelope::success(
                "quarter_advise",
                payload::quarter_advise_data(&qid, capacity, &advice)
            )
            .to_json()
        );
        return;
    }

    println!(
        "{qid} · advise next — {} candidate(s), capacity {capacity}",
        advice.len()
    );
    if goals.is_empty() {
        println!("  (no open quarter goals — ranked on priority + attention only)");
    }
    if advice.is_empty() {
        println!("  No pending backlog candidates.");
        return;
    }
    let mut cut_drawn = false;
    for a in &advice {
        if !a.within_capacity && !cut_drawn {
            println!("  ── below capacity ──");
            cut_drawn = true;
        }
        println!(
            "  {}. [{}] {} ({}, score {})",
            a.rank, a.repo, a.title, a.kind, a.score
        );
        println!("      {}", a.rationale);
    }
}
