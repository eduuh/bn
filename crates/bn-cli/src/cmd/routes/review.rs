use crate::*;

mod decompose;

pub(super) fn route(cfg: &Config, action: CmdReview, json: bool) {
    let timer = bn_core::log::enabled().then(bn_core::log::Timer::start);
    bn_core::log::action(
        "review",
        "route",
        serde_json::json!({ "action": format!("{action:?}"), "json": json }),
    );
    match action {
        CmdReview::Orchestrate {
            action,
            queue,
            repo,
            concurrency,
            coding,
            research,
            solo,
            agent,
            foreground,
        } => {
            let ctx = resolver::resolve(cfg, &std::env::current_dir().unwrap_or_default()).ok();
            match action {
                None if !queue.is_empty() => {
                    if !orchestrate_start(
                        cfg,
                        ctx.as_ref(),
                        repo.as_deref(),
                        &queue,
                        concurrency,
                        coding,
                        research,
                        solo,
                        agent.as_deref(),
                        foreground,
                        json,
                    ) {
                        std::process::exit(1);
                    }
                    if let Some(timer) = timer {
                        bn_core::log::record(
                            bn_core::log::LogLevel::Info,
                            "review",
                            "route_done",
                            serde_json::json!({ "json": json }),
                            serde_json::json!({ "ok": true }),
                            Some(timer.elapsed_ms()),
                        );
                    }
                }
                None => orchestrate_status(cfg, None, false, json),
                Some(OrchestrateCmd::Doctor { repo }) => {
                    if !orchestrate_doctor(cfg, repo.as_deref(), json) {
                        std::process::exit(1);
                    }
                }
                Some(OrchestrateCmd::Status {
                    repo,
                    why,
                    degraded,
                }) => {
                    if degraded {
                        orchestrate_degraded_status(repo.as_deref(), json);
                    } else {
                        orchestrate_status(cfg, repo.as_deref(), why, json);
                    }
                }
                Some(OrchestrateCmd::Logs { repo, level, limit }) => orchestrate_logs(
                    repo.as_deref(),
                    level.map(OrchestrateLogLevelArg::to_core),
                    limit,
                    json,
                ),
                Some(OrchestrateCmd::Watch {
                    repo,
                    interval,
                    plain,
                }) => orchestrate_watch(cfg, repo.as_deref(), interval, plain, json),
                Some(OrchestrateCmd::Run {
                    once,
                    repo,
                    all_repos,
                }) => orchestrate_run(cfg, repo.as_deref(), all_repos, once, json),
                Some(OrchestrateCmd::Reconcile { repo }) => {
                    if !orchestrate_reconcile(cfg, repo.as_deref(), json) {
                        std::process::exit(1);
                    }
                }
                Some(OrchestrateCmd::Reset { lane, repo }) => {
                    if !orchestrate_reset(
                        cfg,
                        ctx.as_ref().map(|c| c.repo.as_str()),
                        repo.as_deref(),
                        &lane,
                        json,
                    ) {
                        std::process::exit(1);
                    }
                }
                Some(OrchestrateCmd::Supervise {
                    repo,
                    all_repos,
                    concurrency,
                    keep_warm,
                    low_water,
                    own,
                    exclude_title,
                    exclude_label,
                    brainstorm,
                    foreground,
                }) => {
                    if !orchestrate_supervise(
                        cfg,
                        ctx.as_ref(),
                        repo.as_deref(),
                        SuperviseOptions {
                            all_repos,
                            concurrency,
                            keep_warm,
                            low_water,
                            owned_repos: own,
                            exclude_titles: exclude_title,
                            exclude_labels: exclude_label,
                            brainstorm_count: brainstorm,
                            foreground,
                            json,
                        },
                    ) {
                        std::process::exit(1);
                    }
                }
                Some(OrchestrateCmd::Stop { repo, all_repos }) => orchestrate_stop(
                    cfg,
                    repo.as_deref(),
                    all_repos,
                    ctx.as_ref().map(|c| c.repo.as_str()),
                    json,
                ),
                Some(OrchestrateCmd::Scale {
                    repo,
                    coding,
                    research,
                }) => {
                    if !orchestrate_scale(
                        cfg,
                        ctx.as_ref().map(|c| c.repo.as_str()),
                        repo.as_deref(),
                        coding,
                        research,
                        json,
                    ) {
                        std::process::exit(1);
                    }
                }
                Some(OrchestrateCmd::Add {
                    items,
                    repo,
                    priority,
                    kind,
                    touches,
                    substrate,
                    source,
                    start,
                    concurrency,
                    solo,
                }) => {
                    if !orchestrate_add(
                        cfg,
                        ctx.as_ref(),
                        repo.as_deref(),
                        &items,
                        bn_core::backlog::AddSpec {
                            kind: kind.to_core(),
                            priority,
                            touches,
                            substrate: substrate.to_core(),
                            source: source.unwrap_or_default(),
                            controller_id: std::env::var("BN_CONTROLLER_ID").ok(),
                        },
                        start,
                        concurrency,
                        solo,
                        json,
                    ) {
                        std::process::exit(1);
                    }
                }
                Some(OrchestrateCmd::Ask {
                    question,
                    repo,
                    wait,
                }) => {
                    if !orchestrate_ask(
                        cfg,
                        ctx.as_ref(),
                        repo.as_deref(),
                        &question.join(" "),
                        wait,
                        json,
                    ) {
                        std::process::exit(1);
                    }
                }
                Some(OrchestrateCmd::Answers { repo, id }) => {
                    if !orchestrate_answers(cfg, ctx.as_ref(), repo.as_deref(), id.as_deref(), json)
                    {
                        std::process::exit(1);
                    }
                }
                Some(OrchestrateCmd::Backlog {
                    repo,
                    all_repos,
                    status,
                    limit,
                }) => {
                    if !orchestrate_backlog(
                        cfg,
                        ctx.as_ref(),
                        repo.as_deref(),
                        all_repos,
                        status.to_filter(),
                        limit,
                        json,
                    ) {
                        std::process::exit(1);
                    }
                }
                Some(OrchestrateCmd::QueueFrom {
                    check,
                    repo,
                    group_by,
                    priority,
                    dry_run,
                    start,
                    concurrency,
                }) => {
                    if !orchestrate_queue_from(
                        cfg,
                        ctx.as_ref(),
                        repo.as_deref(),
                        &check.join(" "),
                        &group_by,
                        priority,
                        dry_run,
                        start,
                        concurrency,
                        json,
                    ) {
                        std::process::exit(1);
                    }
                }
                Some(OrchestrateCmd::Decompose {
                    issue,
                    part,
                    repo,
                    priority,
                    dry_run,
                }) => {
                    if !decompose::orchestrate_decompose(
                        cfg,
                        ctx.as_ref(),
                        repo.as_deref(),
                        &issue,
                        &part,
                        priority,
                        dry_run,
                        json,
                    ) {
                        std::process::exit(1);
                    }
                }
            }
        }
        CmdReview::ReviewPr { action } => {
            if !review_pr::run(cfg, action, json) {
                std::process::exit(1);
            }
        }
        CmdReview::ReviewGate { action } => {
            let required = cfg.review_diff_gate_reviews;
            match action {
                ReviewGateCmd::Record {
                    repo,
                    number,
                    reviewer,
                    verdict,
                    sha,
                    summary,
                    url,
                    branch,
                    findings,
                } => emit_review_gate_record(
                    &repo,
                    number,
                    &reviewer,
                    &verdict,
                    &sha,
                    url.as_deref(),
                    branch.as_deref(),
                    summary.as_deref(),
                    &findings,
                    required,
                    json,
                ),
                ReviewGateCmd::Resolve {
                    repo,
                    number,
                    finding_id,
                    note,
                } => emit_review_gate_resolve(&repo, number, &finding_id, note.as_deref(), json),
                ReviewGateCmd::Waive {
                    repo,
                    number,
                    finding_id,
                    reason,
                } => emit_review_gate_waive(&repo, number, &finding_id, &reason, json),
                ReviewGateCmd::Status { repo, number, sha } => {
                    emit_review_gate_status(&repo, number, sha.as_deref(), required, json)
                }
                ReviewGateCmd::List => emit_review_gate_list(required, json),
                ReviewGateCmd::Prompt {
                    repo,
                    number,
                    reviewer,
                } => emit_review_gate_prompt(&repo, number, reviewer.as_deref(), required, json),
            }
        }
        CmdReview::Done { query, pct } => {
            let ctx = resolve_or_exit(cfg);
            let res = commands::done(cfg, &ctx, &query.join(" "), &today(), pct);
            emit_done(res, json);
            sync_tmux(cfg, &ctx);
        }
        CmdReview::Ingest { branch, base } => {
            let mut ctx = resolve_or_exit(cfg);
            if let Some(b) = branch {
                ctx.branch = b.replace('/', "-");
            }
            run_ingest(cfg, &ctx, &base, json);
        }
        CmdReview::Goal { text } => {
            let ctx = resolve_or_exit(cfg);
            if text.is_empty() {
                // Read mode: extract the goal from the note body (the nvim plugin reads this).
                let goal = bn_core::store::read(&cfg.note_file(&ctx))
                    .ok()
                    .and_then(|n| bn_core::render::goal_text(&n.body))
                    .unwrap_or_default();
                if json {
                    let env = Envelope::success(
                        "goal",
                        payload::goal_data(&commands::GoalOutcome { goal }),
                    );
                    println!("{}", env.to_json());
                } else if goal.is_empty() {
                    println!("No goal set");
                } else {
                    println!("Goal: {goal}");
                }
            } else {
                let res = commands::set_goal(cfg, &ctx, &text.join(" "), &today());
                emit_goal(res, json);
                sync_tmux(cfg, &ctx);
            }
        }
        CmdReview::Learned { text } => {
            if text.is_empty() {
                fail("bn learned: text required — bn learned \"<what you learned>\"");
            }
            let prov = log_provenance(cfg);
            match bn_core::daily::learned(cfg, &today(), &text.join(" "), &prov) {
                Ok(a) => {
                    if json {
                        println!(
                            "{}",
                            Envelope::success("learned", payload::daily_add_data(&a)).to_json()
                        );
                    } else {
                        println!("Learned [{}]: {}", a.id, a.text);
                    }
                }
                Err(e) => emit_err("learned", &e, json),
            }
        }
        CmdReview::Reflect { text } => {
            if text.is_empty() {
                fail("bn reflect: text required — bn reflect \"<one thing to improve>\"");
            }
            let prov = log_provenance(cfg);
            match bn_core::daily::reflect(cfg, &today(), &text.join(" "), &prov) {
                Ok(a) => {
                    if json {
                        println!(
                            "{}",
                            Envelope::success("reflect", payload::daily_add_data(&a)).to_json()
                        );
                    } else {
                        println!("Reflection [{}]: {}", a.id, a.text);
                    }
                }
                Err(e) => emit_err("reflect", &e, json),
            }
        }
        CmdReview::Learnings {
            action,
            since,
            all,
            repo,
            skill,
            mine,
            agent,
        } => match action {
            None => {
                let author = if mine {
                    bn_core::learnings::Author::Mine
                } else if agent {
                    bn_core::learnings::Author::Agent
                } else {
                    bn_core::learnings::Author::Any
                };
                let filter = bn_core::learnings::Filter {
                    since,
                    all,
                    repo,
                    skill,
                    author,
                };
                emit_learnings(cfg, &filter, json);
            }
            Some(LearningsCmd::Promote { query, note }) => emit_learnings_review(
                cfg,
                &query.join(" "),
                bn_core::learnings::Review::Promoted,
                note.as_deref(),
                json,
            ),
            Some(LearningsCmd::Dismiss { query, note }) => emit_learnings_review(
                cfg,
                &query.join(" "),
                bn_core::learnings::Review::Dismissed,
                note.as_deref(),
                json,
            ),
        },
        CmdReview::Note => {
            let ctx = resolve_or_exit(cfg);
            emit_note(commands::note_path(cfg, &ctx, &today()), json);
        }
        CmdReview::Cat => {
            let ctx = resolve_or_exit(cfg);
            emit_cat(cfg, &ctx);
        }
        CmdReview::Review { action } => match action {
            None | Some(ReviewCmd::List) => emit_review_list(json),
            Some(ReviewCmd::Submit {
                repo,
                branch,
                session,
                plan,
                plan_file,
                reviewers,
                quorum,
                labels,
                paths,
            }) => emit_review_submit(
                &repo,
                &branch,
                session.as_deref(),
                plan,
                plan_file,
                ReviewConsensusArgs {
                    reviewers,
                    quorum,
                    labels,
                    paths,
                },
                json,
            ),
            Some(ReviewCmd::Status { id }) => emit_review_status(&id, json),
            Some(ReviewCmd::Show { id, reviewer }) => {
                emit_review_show(&id, reviewer.as_deref(), json)
            }
            Some(ReviewCmd::Agents {
                id,
                harness,
                model,
                add,
                kind,
            }) => emit_review_agents(&id, &harness, model.as_deref(), add, &kind, json),
            Some(ReviewCmd::Vote {
                id,
                reviewer,
                approve,
                reject,
                confidence,
                notes,
                risks,
            }) => emit_review_vote(
                &id,
                ReviewVoteArgs {
                    reviewer: &reviewer,
                    approve,
                    reject,
                    confidence,
                    notes: notes.as_deref(),
                    risks: &risks,
                },
                json,
            ),
            Some(ReviewCmd::Edit {
                id,
                approve,
                request_changes,
            }) => emit_review_edit(id, approve, request_changes, json),
            Some(ReviewCmd::Approve { id }) => {
                emit_review_decide(&id, bn_core::plan_review::Decision::Approve, None, json)
            }
            Some(ReviewCmd::RequestChanges { id, notes }) => emit_review_decide(
                &id,
                bn_core::plan_review::Decision::RequestChanges,
                Some(&notes),
                json,
            ),
            Some(ReviewCmd::Reject { id }) => {
                emit_review_decide(&id, bn_core::plan_review::Decision::Reject, None, json)
            }
        },
    }
}
