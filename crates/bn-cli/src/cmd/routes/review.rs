use crate::*;

pub(super) fn route(cfg: &Config, action: CmdReview, json: bool) {
    let _timer = bn_core::log::enabled().then(bn_core::log::Timer::start);
    bn_core::log::action(
        "review",
        "route",
        serde_json::json!({ "action": format!("{action:?}"), "json": json }),
    );
    match action {
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
                } => {
                    // Default to the reviewer worktree's HEAD — the checkout just read — so the
                    // critic prompt needn't splice a `$(git rev-parse HEAD)` subshell.
                    let sha = sha.unwrap_or_else(|| {
                        crate::git::git_capture(&["rev-parse", "HEAD"], None, false)
                            .map(|s| s.trim().to_string())
                            .unwrap_or_else(|| {
                                fail("bn review-gate record: --sha not given and `git rev-parse HEAD` failed (not in a git worktree?)")
                            })
                    });
                    emit_review_gate_record(
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
                )
                }
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
                } => {
                    emit_review_gate_prompt(cfg, &repo, number, reviewer.as_deref(), required, json)
                }
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
            }) => emit_review_agents(cfg, &id, &harness, model.as_deref(), add, &kind, json),
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
