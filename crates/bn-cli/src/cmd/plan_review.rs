//! `bn review` — the plan-review gate CLI (#349): the human's approve/request-changes/reject side
//! of a dispatched agent's plan, plus the agent's own `submit`/`status` write/poll. The store lives
//! in `bn_core::plan_review`; this is the thin CLI shell over it.
use crate::*;
use bn_core::plan_review::{
    self, DecideError, Decision, Review, ReviewStatus, VoteError, VoteVerdict,
};
use serde::Deserialize;
use std::time::Duration;

mod reviewer;
use reviewer::{ReviewerKind, ReviewerSpec};

const AGENT_REVIEW_WALL_SECS: u64 = 240;

/// Upper bound on reviewer agents spawned concurrently. A scaled-up panel runs in chunks of this
/// size so we never fork an unbounded number of headless harnesses at once.
const REVIEWER_CONCURRENCY: usize = 5;

#[derive(Debug, Deserialize)]
struct AgentReviewReply {
    #[serde(deserialize_with = "deserialize_vote_verdict")]
    verdict: VoteVerdict,
    confidence: u8,
    reasoning: String,
    #[serde(default)]
    risks: Vec<String>,
}

struct AgentReviewRun {
    reviewer: String,
    reply: Result<AgentReviewReply, String>,
}

pub(crate) struct ReviewConsensusArgs {
    pub reviewers: usize,
    pub quorum: Option<usize>,
    pub labels: Vec<String>,
    pub paths: Vec<String>,
}

pub(crate) struct ReviewVoteArgs<'a> {
    pub reviewer: &'a str,
    pub approve: bool,
    pub reject: bool,
    pub confidence: Option<u8>,
    pub notes: Option<&'a str>,
    pub risks: &'a [String],
}

/// Read the plan text from `--plan`, `--plan-file` (with `-` meaning stdin), or bare stdin. Trims
/// trailing whitespace; a genuinely empty plan is rejected so a review always carries something.
fn read_plan(plan: Option<String>, plan_file: Option<String>) -> String {
    use std::io::Read;
    let text = match (plan, plan_file) {
        (Some(p), _) => p,
        (None, Some(f)) if f != "-" => std::fs::read_to_string(&f).unwrap_or_else(|e| {
            fail(&format!(
                "bn review submit: cannot read --plan-file '{f}': {e}"
            ))
        }),
        _ => {
            let mut s = String::new();
            if std::io::stdin().read_to_string(&mut s).is_err() {
                fail("bn review submit: failed to read the plan from stdin");
            }
            s
        }
    };
    let text = text.trim_end().to_string();
    if text.trim().is_empty() {
        fail("bn review submit: empty plan — pass --plan, --plan-file, or pipe it on stdin");
    }
    text
}

/// `bn review submit --repo R --branch B [--session S] [--plan… ]` — the agent registers (or
/// re-submits) a plan and pauses. Prints the review id + status.
pub(crate) fn emit_review_submit(
    repo: &str,
    branch: &str,
    session: Option<&str>,
    plan: Option<String>,
    plan_file: Option<String>,
    consensus: ReviewConsensusArgs,
    as_json: bool,
) {
    let timer = bn_core::log::enabled().then(bn_core::log::Timer::start);
    let plan = read_plan(plan, plan_file);
    let now = bn_core::util::now_epoch_secs();
    let result = if consensus.reviewers == 0 {
        plan_review::submit(repo, branch, session, &plan, now)
    } else {
        let quorum = consensus
            .quorum
            .unwrap_or_else(|| plan_review::majority_quorum(consensus.reviewers));
        plan_review::submit_with_consensus(
            repo,
            branch,
            session,
            &plan,
            consensus.reviewers,
            quorum,
            &consensus.labels,
            &consensus.paths,
            now,
        )
    };
    match result {
        Ok(review) => {
            bn_core::log::record(
                bn_core::log::LogLevel::Info,
                "review",
                "submit",
                serde_json::json!({
                    "repo": repo,
                    "branch": branch,
                    "session": session,
                    "reviewers": consensus.reviewers,
                    "quorum": review.quorum,
                    "json": as_json,
                }),
                serde_json::json!({ "review_id": review.id, "status": review.status.label() }),
                timer.as_ref().map(|t| t.elapsed_ms()),
            );
            if as_json {
                print_json(&review);
            } else {
                println!(
                    "plan-review {} raised ({}) for {}/{} — {}",
                    review.id,
                    review.status.label(),
                    review.repo,
                    review.branch,
                    if review.reviewer_count == 0 {
                        "paused for human approval".to_string()
                    } else {
                        format!(
                            "awaiting {}/{} independent approvals{}",
                            review.quorum,
                            review.reviewer_count,
                            if review.high_risk {
                                " plus mandatory human approval"
                            } else {
                                ""
                            }
                        )
                    }
                );
                println!("  poll:    bn review status {}", review.id);
                println!(
                    "  approve: bn review approve {} (or the dashboard)",
                    review.id
                );
            }
        }
        Err(e) => fail(&format!("bn review submit: {e}")),
    }
}

/// `bn review status <id>` — the agent's poll read. Prints just the status label (so a shell loop
/// can branch on it); `--json` prints the full record.
pub(crate) fn emit_review_status(id: &str, as_json: bool) {
    match plan_review::get(id) {
        Some(review) => {
            if bn_core::log::enabled() {
                bn_core::log::record(
                    bn_core::log::LogLevel::Info,
                    "review",
                    "status",
                    serde_json::json!({ "id": id }),
                    serde_json::json!({
                        "status": review.status.label(),
                        "consensus": review.consensus.label(),
                        "reviewers": review.reviewer_count,
                    }),
                    None,
                );
            }
            if as_json {
                print_json(&review.reviewer_view());
            } else {
                println!("{}", review.status.label());
                if review.reviewer_count > 0 {
                    println!("consensus: {}", review.consensus.label());
                }
                if let Some(notes) = review
                    .notes
                    .as_deref()
                    .filter(|_| review.status == ReviewStatus::ChangesRequested)
                {
                    println!("notes: {notes}");
                }
            }
        }
        None => {
            if as_json {
                println!(
                    "{}",
                    Envelope::failure("review", "not_found", format!("no plan-review '{id}'"))
                        .to_json()
                );
            } else {
                eprintln!("bn review status: no plan-review '{id}'");
            }
            std::process::exit(1);
        }
    }
}

/// `bn review list` (and bare `bn review`) — every plan-review, newest first, pending ones flagged.
pub(crate) fn emit_review_list(as_json: bool) {
    let reviews = plan_review::list();
    if as_json {
        let reviews: Vec<Review> = reviews.iter().map(Review::reviewer_view).collect();
        println!(
            "{}",
            Envelope::success("review", payload::plan_reviews_data(&reviews)).to_json()
        );
        return;
    }
    if reviews.is_empty() {
        println!("No plan-reviews — a `bn dispatch --plan` agent raises one before it ships.");
        return;
    }
    for r in &reviews {
        let flag = if r.status.is_pending() {
            format!("{} needs you", bn_core::glyph::CLOCK)
        } else {
            r.status.label().to_string()
        };
        println!(
            "[{}] {}  {}/{}  — {flag}",
            r.id,
            r.status.label(),
            r.repo,
            r.branch
        );
        if let Some(first) = r.plan.lines().find(|l| !l.trim().is_empty()) {
            println!("  {}", first.trim());
        }
        if r.status == ReviewStatus::ChangesRequested {
            if let Some(notes) = &r.notes {
                println!("  notes: {notes}");
            }
        }
    }
    let pending = reviews.iter().filter(|r| r.status.is_pending()).count();
    if pending > 0 {
        println!("\n{pending} pending — `bn review approve <id>` / `request-changes <id> --notes` / `reject <id>`");
    }
}

/// `bn review show <id>` — one review in full, plan text included.
pub(crate) fn emit_review_show(id: &str, reviewer: Option<&str>, as_json: bool) {
    match plan_review::get(id) {
        Some(review) => {
            let visible = if reviewer.is_some() {
                review.reviewer_view()
            } else {
                review
            };
            if as_json {
                print_full_review_json(&visible);
            } else {
                render_review(&visible);
            }
        }
        None => {
            if as_json {
                println!(
                    "{}",
                    Envelope::failure("review", "not_found", format!("no plan-review '{id}'"))
                        .to_json()
                );
            } else {
                eprintln!("bn review show: no plan-review '{id}'");
            }
            std::process::exit(1);
        }
    }
}

pub(crate) fn emit_review_vote(id: &str, args: ReviewVoteArgs<'_>, as_json: bool) {
    let verdict = match (args.approve, args.reject) {
        (true, false) => VoteVerdict::Approve,
        (false, true) => VoteVerdict::Reject,
        _ => fail("bn review vote: pass exactly one of --approve or --reject"),
    };
    match plan_review::vote_with_feedback(
        id,
        args.reviewer,
        verdict,
        args.confidence,
        args.notes,
        args.risks,
        bn_core::util::now_epoch_secs(),
    ) {
        Ok(review) => {
            if as_json {
                print_json(&review.reviewer_view());
            } else {
                println!(
                    "plan-review {}: {} voted {} — consensus {} ({}/{})",
                    review.id,
                    args.reviewer,
                    verdict.label(),
                    review.consensus.label(),
                    review.approvals(),
                    review.quorum
                );
            }
        }
        Err(e) => {
            let code = match &e {
                VoteError::NotFound(_) => "not_found",
                VoteError::NotPending { .. } => "not_pending",
                VoteError::NotConsensusReview(_) => "human_only",
                VoteError::NothingToAdd(_) => "nothing_to_add",
                VoteError::DuplicateReviewer(_) => "duplicate_reviewer",
                VoteError::VotingClosed(_) => "voting_closed",
                VoteError::EmptyReviewer => "empty_reviewer",
                VoteError::InvalidConfidence(_) => "invalid_confidence",
                VoteError::Io(_) => "io",
            };
            if as_json {
                println!(
                    "{}",
                    Envelope::failure("review", code, e.to_string()).to_json()
                );
            } else {
                eprintln!("bn review vote: {e}");
            }
            std::process::exit(1);
        }
    }
}

pub(crate) fn emit_review_agents(
    cfg: &Config,
    id: &str,
    harness: &str,
    model: Option<&str>,
    add: Option<usize>,
    kinds: &[String],
    as_json: bool,
) {
    let review = plan_review::get(id)
        .unwrap_or_else(|| fail(&format!("bn review agents: no plan-review '{id}'")));
    if !review.status.is_pending() {
        fail(&format!(
            "bn review agents: plan-review '{}' is {} — not pending",
            review.id,
            review.status.label()
        ));
    }
    if review.reviewer_count == 0 {
        fail(&format!(
            "bn review agents: plan-review '{}' is human-only; submit it with --reviewers",
            review.id
        ));
    }

    // `--add`/`--kind` scale the panel up on demand (e.g. a deadlocked no-consensus review):
    // grow the ballot count, then spawn exactly that many typed critics. A bare invocation just
    // fills the currently-unfilled slots with generic critics, as before.
    let (review, specs) = if add.is_some() || !kinds.is_empty() {
        let kinds =
            resolve_kinds(add, kinds).unwrap_or_else(|e| fail(&format!("bn review agents: {e}")));
        let grown = plan_review::add_reviewers(id, kinds.len(), bn_core::util::now_epoch_secs())
            .unwrap_or_else(|e| fail(&format!("bn review agents: {e}")));
        let specs = typed_specs(&grown, &kinds);
        (grown, specs)
    } else {
        let specs = default_specs(&review);
        (review, specs)
    };

    let runs = run_review_agents(cfg, &review, &specs, harness, model);
    if runs.is_empty() {
        if as_json {
            print_full_review_json(&review);
        } else {
            println!(
                "plan-review {} already has all {} configured ballots",
                review.id, review.reviewer_count
            );
        }
        return;
    }

    let mut failures = Vec::new();
    let mut latest = review;
    for run in runs {
        match run.reply {
            Ok(reply) => match plan_review::vote_with_feedback(
                &latest.id,
                &run.reviewer,
                reply.verdict,
                Some(reply.confidence),
                Some(&reply.reasoning),
                &reply.risks,
                bn_core::util::now_epoch_secs(),
            ) {
                Ok(updated) => {
                    if !as_json {
                        println!(
                            "{}: {} ({}%)",
                            run.reviewer,
                            reply.verdict.label(),
                            reply.confidence
                        );
                    }
                    latest = updated;
                }
                Err(e) => failures.push(format!("{}: could not record verdict: {e}", run.reviewer)),
            },
            Err(e) => failures.push(format!("{}: {e}", run.reviewer)),
        }
    }

    if as_json {
        if failures.is_empty() {
            print_full_review_json(&latest);
        } else {
            println!(
                "{}",
                Envelope::failure("review", "agent_review_failed", failures.join("; ")).to_json()
            );
        }
    } else {
        println!(
            "plan-review {}: consensus {} ({}/{})",
            latest.id,
            latest.consensus.label(),
            latest.approvals(),
            latest.quorum
        );
        for failure in &failures {
            eprintln!("bn review agents: {failure}");
        }
    }
    if !failures.is_empty() {
        std::process::exit(1);
    }
}

/// Allocate `count` fresh reviewer identities of `kind`, skipping any name already present in
/// `taken` (case-insensitive) so a re-run never collides with a recorded ballot or a peer spawned
/// in the same batch. Names use a per-prefix running index (`rubber-duck-1`, `rubber-duck-2`, …).
fn allocate_specs(kind: ReviewerKind, count: usize, taken: &mut Vec<String>) -> Vec<ReviewerSpec> {
    let mut specs = Vec::with_capacity(count);
    let mut index = 1;
    for _ in 0..count {
        let reviewer = loop {
            let candidate = format!("{}-{index}", kind.slug());
            index += 1;
            if !taken
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&candidate))
            {
                break candidate;
            }
        };
        taken.push(reviewer.clone());
        specs.push(ReviewerSpec { reviewer, kind });
    }
    specs
}

/// Generic reviewers for every currently-unfilled ballot — the historical `bn review agents`
/// behavior, preserved for a bare invocation.
fn default_specs(review: &Review) -> Vec<ReviewerSpec> {
    let unfilled = review.reviewer_count.saturating_sub(review.votes.len());
    let mut taken: Vec<String> = review.votes.iter().map(|v| v.reviewer.clone()).collect();
    allocate_specs(ReviewerKind::Generic, unfilled, &mut taken)
}

/// Concrete typed reviewers to spawn for a scale-up, one per resolved kind, named so they never
/// clash with ballots already on the review.
fn typed_specs(review: &Review, kinds: &[ReviewerKind]) -> Vec<ReviewerSpec> {
    let mut taken: Vec<String> = review.votes.iter().map(|v| v.reviewer.clone()).collect();
    let mut specs = Vec::with_capacity(kinds.len());
    for &kind in kinds {
        specs.extend(allocate_specs(kind, 1, &mut taken));
    }
    specs
}

/// Resolve the `--add N` / `--kind …` request into the exact specialties to spawn. `--add` counts
/// reviewers; `--kind` (repeatable) names their specialty. With only `--kind` the count defaults to
/// the number of kinds; with only `--add` the reviewers are rubber-ducks. Kinds cycle when fewer
/// are named than reviewers requested.
fn resolve_kinds(add: Option<usize>, kinds: &[String]) -> Result<Vec<ReviewerKind>, String> {
    let parsed: Vec<ReviewerKind> = kinds
        .iter()
        .map(|raw| {
            ReviewerKind::parse(raw).ok_or_else(|| {
                format!("unknown reviewer kind '{raw}' (use: rubber-duck | code-review)")
            })
        })
        .collect::<Result<_, _>>()?;

    let count = add.unwrap_or(parsed.len());
    if count == 0 {
        return Err("nothing to add: pass --add N (N > 0) or at least one --kind".to_string());
    }

    let resolved = (0..count)
        .map(|i| {
            if parsed.is_empty() {
                ReviewerKind::RubberDuck
            } else {
                parsed[i % parsed.len()]
            }
        })
        .collect();
    Ok(resolved)
}

fn run_review_agents(
    cfg: &Config,
    review: &Review,
    specs: &[ReviewerSpec],
    harness: &str,
    model: Option<&str>,
) -> Vec<AgentReviewRun> {
    let mut runs = Vec::with_capacity(specs.len());
    // Chunk the fan-out so at most REVIEWER_CONCURRENCY harnesses run at once; extra reviewers
    // queue behind the current chunk instead of forking an unbounded thread pool.
    for chunk in specs.chunks(REVIEWER_CONCURRENCY) {
        let chunk_runs = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for spec in chunk {
                let command = agent_review_prompt(cfg, review, spec).and_then(|prompt| {
                    crate::cmd::staff::staff_dispatch_command(harness, &prompt, model, None)
                        .ok_or_else(|| format!("unknown harness '{harness}' (use: copilot | claude)"))
                });
                handles.push((
                    spec.reviewer.clone(),
                    command.map(|command| {
                        scope.spawn(move || {
                            crate::cmd::staff::dispatch_capture(
                                command,
                                Some(Duration::from_secs(AGENT_REVIEW_WALL_SECS)),
                            )
                        })
                    }),
                ));
            }

            handles
                .into_iter()
                .map(|(reviewer, handle)| {
                    let reply = match handle {
                        Err(error) => Err(error),
                        Ok(handle) => match handle.join() {
                            Err(_) => Err("reviewer process panicked".to_string()),
                            Ok((false, _, combined)) => {
                                Err(format!("reviewer process failed: {}", combined.trim()))
                            }
                            Ok((true, stdout, _)) => parse_agent_review_reply(&stdout),
                        },
                    };
                    AgentReviewRun { reviewer, reply }
                })
                .collect::<Vec<_>>()
        });
        runs.extend(chunk_runs);
    }
    runs
}

fn agent_review_prompt(
    cfg: &Config,
    review: &Review,
    spec: &ReviewerSpec,
) -> Result<String, String> {
    let base = cfg.worktree_dir.join(&review.repo).join(&review.branch);
    let base = if base.is_dir() {
        base
    } else {
        std::env::current_dir().unwrap_or_default()
    };
    let mut args = serde_json::Map::new();
    for (name, value) in [
        ("reviewer", spec.reviewer.clone()),
        ("lens", spec.kind.lens().to_string()),
        ("repo", review.repo.clone()),
        ("branch", review.branch.clone()),
        ("review_id", review.id.clone()),
        ("plan", review.plan.clone()),
    ] {
        args.insert(name.into(), serde_json::Value::String(value));
    }
    bn_core::skills::render_required(cfg, &base, "plan-review-critic", Some(&args))
        .map_err(|error| error.to_string())
}

/// Extract the reviewer's final JSON verdict from headless-harness stdout. Headless `copilot -p`
/// prints its tool-use trace (with stray `{`/`}` from code snippets) before the answer, so we can't
/// just slice from the first `{`: we anchor on the last `}` and try candidate `{` openers from the
/// rightmost inward, returning the first slice that parses as a complete verdict. Braces are ASCII,
/// so byte-index slicing stays on char boundaries.
fn parse_agent_review_reply(stdout: &str) -> Result<AgentReviewReply, String> {
    let end = stdout
        .rfind('}')
        .ok_or_else(|| "reviewer returned no JSON object".to_string())?;
    let bytes = stdout.as_bytes();
    let mut opens: Vec<usize> = (0..=end).filter(|&i| bytes[i] == b'{').collect();
    if opens.is_empty() {
        return Err("reviewer returned no JSON object".to_string());
    }
    opens.sort_unstable_by(|a, b| b.cmp(a));

    let mut last_err = "reviewer returned an incomplete JSON object".to_string();
    for start in opens {
        match serde_json::from_str::<AgentReviewReply>(&stdout[start..=end]) {
            Ok(reply) => {
                if reply.confidence > 100 {
                    return Err(format!(
                        "reviewer confidence must be between 0 and 100, got {}",
                        reply.confidence
                    ));
                }
                if reply.reasoning.trim().is_empty() {
                    return Err("reviewer reasoning cannot be empty".to_string());
                }
                return Ok(reply);
            }
            Err(e) => last_err = format!("invalid reviewer JSON: {e}"),
        }
    }
    Err(last_err)
}

fn deserialize_vote_verdict<'de, D>(deserializer: D) -> Result<VoteVerdict, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    VoteVerdict::parse(&value)
        .ok_or_else(|| serde::de::Error::custom(format!("unknown verdict '{value}'")))
}

/// `bn review approve|request-changes|reject <id>` — the human's decision on a pending review.
pub(crate) fn emit_review_decide(id: &str, decision: Decision, notes: Option<&str>, as_json: bool) {
    let timer = bn_core::log::enabled().then(bn_core::log::Timer::start);
    let now = bn_core::util::now_epoch_secs();
    match plan_review::decide(id, decision, notes, now) {
        Ok(review) => {
            bn_core::log::record(
                bn_core::log::LogLevel::Info,
                "review",
                "decide",
                serde_json::json!({
                    "id": id,
                    "decision": format!("{decision:?}"),
                    "json": as_json,
                }),
                serde_json::json!({ "status": review.status.label() }),
                timer.as_ref().map(|t| t.elapsed_ms()),
            );
            // On approval, persist the signed-off plan into the branch note (#648).
            let recorded = if decision == Decision::Approve {
                record_approved_plan_to_note(&review)
            } else {
                false
            };
            if as_json {
                print_json(&review);
            } else {
                let verb = match decision {
                    Decision::Approve => "approved — the agent may implement + open the PR",
                    Decision::RequestChanges => {
                        "changes requested — the agent will revise + re-submit"
                    }
                    Decision::Reject => "rejected — the agent stops",
                };
                println!("plan-review {} {verb}", review.id);
                if recorded {
                    println!(
                        "  saved the plan to {}/{}'s note (## Plan)",
                        review.repo, review.branch
                    );
                }
            }
        }
        Err(e) => {
            let code = match &e {
                DecideError::NotFound(_) => "not_found",
                DecideError::NotPending { .. } => "not_pending",
                DecideError::MissingNotes => "missing_notes",
                DecideError::EmptyPlan => "empty_plan",
                DecideError::Io(_) => "io",
            };
            if as_json {
                println!(
                    "{}",
                    Envelope::failure("review", code, e.to_string()).to_json()
                );
            } else {
                eprintln!("bn review: {e}");
            }
            std::process::exit(1);
        }
    }
}

fn print_json(review: &Review) {
    let review = review.reviewer_view();
    println!(
        "{}",
        Envelope::success("review", payload::plan_review_data(&review)).to_json()
    );
}

fn print_full_review_json(review: &Review) {
    println!(
        "{}",
        Envelope::success("review", serde_json::json!({ "review": review })).to_json()
    );
}

/// Persist an approved review's plan into its branch note as a verbatim `## Plan` section, so the
/// plan the human signed off on lands in `bn notes` (#648). Best-effort — a note-write failure must
/// never undo the approval. Returns whether the plan was recorded.
fn record_approved_plan_to_note(review: &Review) -> bool {
    let cfg = Config::load();
    let Some(ctx) = target_ctx(&cfg, Some(review.repo.clone()), Some(review.branch.clone())) else {
        return false;
    };
    let today = bn_core::util::today();
    match bn_core::commands::record_plan(&cfg, &ctx, &review.plan, &today) {
        Ok(()) => true,
        Err(e) => {
            eprintln!(
                "bn review: approved, but couldn't save the plan into the note: {}",
                e.message()
            );
            false
        }
    }
}

/// The full-detail text view of one plan-review.
fn render_review(r: &Review) {
    print!("{}", review_text(r));
}

fn review_text(r: &Review) -> String {
    use std::fmt::Write;

    let mut out = String::new();
    let _ = writeln!(out, "plan-review {}  [{}]", r.id, r.status.label());
    let _ = writeln!(out, "  repo:    {}/{}", r.repo, r.branch);
    if let Some(s) = &r.session {
        let _ = writeln!(out, "  session: {s}");
    }
    if let Some(notes) = &r.notes {
        let _ = writeln!(out, "  notes:   {notes}");
    }
    if r.reviewer_count > 0 {
        let _ = writeln!(
            out,
            "  consensus: {} ({}/{} approvals; {} reviewers)",
            r.consensus.label(),
            r.approvals(),
            r.quorum,
            r.reviewer_count
        );
        if r.high_risk {
            let _ = writeln!(
                out,
                "  risk: mandatory human approval ({})",
                r.risk_reasons.join(", ")
            );
        }
        for vote in &r.votes {
            let confidence = vote
                .confidence
                .map(|value| format!(" · confidence {value}%"))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "  reviewer {}: {}{} · {}",
                vote.reviewer,
                vote.verdict.label(),
                confidence,
                vote.created
            );
            if let Some(notes) = &vote.notes {
                let _ = writeln!(out, "    {notes}");
            }
            for risk in &vote.risks {
                let _ = writeln!(out, "    risk: {risk}");
            }
        }
    }
    let _ = writeln!(out, "\n{}\n", r.plan.trim_end());
    match r.status {
        ReviewStatus::Pending => {
            let _ = writeln!(
                out,
                "Pending — `bn review approve {}` / `request-changes {} --notes …` / `reject {}`",
                r.id, r.id, r.id
            );
        }
        ReviewStatus::ChangesRequested => {
            let _ = writeln!(
                out,
                "Changes requested — waiting for the agent to revise + re-submit."
            );
        }
        ReviewStatus::Approved => {
            let _ = writeln!(out, "Approved — the agent may implement + open the PR.");
        }
        ReviewStatus::Rejected => {
            let _ = writeln!(out, "Rejected — the agent stopped.");
        }
    }
    out
}

/// `bn review edit [id]` — open a plan in `$EDITOR` (nvim by default), save the edits back to the
/// review store, then decide. With no id it edits the newest *open* (pending/changes-requested)
/// review — the one you're most likely reviewing. `--approve` / `--request-changes <notes>` skip
/// the interactive prompt (for scripting); otherwise a bare invocation prompts. Bound to `prefix E`
/// in tmux so a pending plan opens in an nvim popup from any pane.
pub(crate) fn emit_review_edit(
    id: Option<String>,
    approve: bool,
    request_changes: Option<String>,
    as_json: bool,
) {
    // Resolve which review to edit: the given id, else the newest open one.
    let review = match id {
        Some(id) => plan_review::get(&id)
            .unwrap_or_else(|| fail(&format!("bn review edit: no plan-review '{id}'"))),
        None => match plan_review::open_reviews().into_iter().next() {
            Some(r) => r,
            None => {
                println!("No open plan-reviews to edit — see `bn review list`.");
                return;
            }
        },
    };
    if review.status.is_terminal() {
        fail(&format!(
            "bn review edit: plan-review {} is {} — not editable",
            review.id,
            review.status.label()
        ));
    }

    // Round-trip the plan through a temp file the editor owns.
    let path = std::env::temp_dir().join(format!("bn-plan-review-{}.md", review.id));
    if let Err(e) = std::fs::write(&path, &review.plan) {
        fail(&format!(
            "bn review edit: cannot write {}: {e}",
            path.display()
        ));
    }
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nvim".into());
    let ran = std::process::Command::new(&editor).arg(&path).status();
    let edited = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    if ran.map(|s| !s.success()).unwrap_or(true) {
        // Editor missing or non-zero exit — don't guess at intent; leave the review untouched.
        fail(&format!(
            "bn review edit: editor '{editor}' did not exit cleanly — review {} unchanged",
            review.id
        ));
    }

    // Persist the edit only when the plan actually changed.
    let now = bn_core::util::now_epoch_secs();
    let changed = edited.trim_end() != review.plan.trim_end();
    if changed {
        if edited.trim().is_empty() {
            fail("bn review edit: the edited plan is empty — review left unchanged");
        }
        match plan_review::update_plan(&review.id, &edited, now) {
            Ok(_) => println!("plan-review {} plan updated", review.id),
            Err(e) => fail(&format!("bn review edit: {e}")),
        }
    } else {
        println!("plan-review {} plan unchanged", review.id);
    }

    // Decide: explicit flags win; otherwise prompt (unless piped, where we just leave it pending).
    let decision = if approve {
        Some((Decision::Approve, None))
    } else if let Some(notes) = request_changes {
        Some((Decision::RequestChanges, Some(notes)))
    } else {
        prompt_decision()
    };
    match decision {
        Some((Decision::Approve, _)) => {
            emit_review_decide(&review.id, Decision::Approve, None, as_json)
        }
        Some((Decision::RequestChanges, notes)) => {
            let notes = notes.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| {
                fail("bn review edit: request-changes needs notes for the agent")
            });
            emit_review_decide(&review.id, Decision::RequestChanges, Some(&notes), as_json)
        }
        Some((Decision::Reject, _)) => {
            emit_review_decide(&review.id, Decision::Reject, None, as_json)
        }
        None => println!(
            "plan-review {} left pending — approve with `bn review approve {}`.",
            review.id, review.id
        ),
    }
}

/// Interactive approve/request-changes/reject/leave-pending prompt for `bn review edit`. Returns
/// `None` (leave pending) when stdin isn't a terminal or the user declines to decide.
fn prompt_decision() -> Option<(Decision, Option<String>)> {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return None;
    }
    print!("Decide: [a]pprove / [r]equest-changes / re[j]ect / [Enter] leave pending: ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
        return None;
    }
    match line.trim().to_lowercase().as_str() {
        "a" | "approve" => Some((Decision::Approve, None)),
        "j" | "reject" => Some((Decision::Reject, None)),
        "r" | "request-changes" | "request" => {
            print!("Notes for the agent (what to change): ");
            let _ = std::io::stdout().flush();
            let mut notes = String::new();
            std::io::stdin().read_line(&mut notes).ok()?;
            Some((Decision::RequestChanges, Some(notes.trim().to_string())))
        }
        _ => None,
    }
}

/// Whether an attention item is the mirror of a pending plan-review — the `plan-review` source (or
/// legacy `mode = "review"`) the gate raises on submit ([`bn_core::plan_review`]). Such a row opens
/// the plan in an nvim popup (`bn review edit`) instead of a browser link.
pub(crate) fn is_plan_review_attn(item: &bn_core::attention::AttnItem) -> bool {
    item.source == "plan-review" || item.mode.as_deref() == Some("review")
}

/// The plan-review id an attention `id` mirrors, or `None` when the id isn't a plan-review row. The
/// gate keys the mirrored item by the review id ([`bn_core::plan_review`] `raise_attn`), so the
/// item's `key` *is* the review id — that's what `bn review edit <id>` / `show <id>` take.
pub(crate) fn plan_review_id_for_attn(attn_id: &str) -> Option<String> {
    let item = bn_core::attention::get(attn_id)?;
    if !is_plan_review_attn(&item) {
        return None;
    }
    item.key
}

/// The tmux `display-popup` argv that opens a plan-review in an nvim popup for decision-in-place,
/// mirroring the `prefix e` / `prefix E` pattern (`-E -w 97% -h 97%`, so closing nvim closes the
/// popup). `bn` is the already-shell-quoted path to this executable; the trailing element is the
/// single shell-command tmux runs (`<bn> review edit <id>` — the read/edit plan buffer plus the
/// approve/request-changes/reject prompt on close). Pure over its inputs → unit-testable.
pub(crate) fn plan_review_popup_cmd(bn: &str, review_id: &str) -> Vec<String> {
    vec![
        "display-popup".into(),
        "-E".into(),
        "-w".into(),
        "97%".into(),
        "-h".into(),
        "97%".into(),
        format!("{bn} review edit {}", crate::shq(review_id)),
    ]
}

/// Open a plan-review attention row: the plan in an nvim popup (`bn review edit <id>`) rather than a
/// browser link. Inside tmux it's a `display-popup` (clean return, mirroring `prefix e`); with no
/// tmux it falls back to running `bn review edit` directly in the current terminal. Returns `true`
/// when `attn_id` was a plan-review and got handled, `false` to let the caller fall back to its
/// default (link/start) action.
pub(crate) fn open_plan_review_popup(attn_id: &str) -> bool {
    let Some(review_id) = plan_review_id_for_attn(attn_id) else {
        return false;
    };
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    if std::env::var_os("TMUX").is_some() {
        let bn = crate::shq(&exe.display().to_string());
        let _ = bn_core::tmux::tmux_run(&plan_review_popup_cmd(&bn, &review_id));
    } else {
        let _ = std::process::Command::new(&exe)
            .args(["review", "edit", &review_id])
            .status();
    }
    true
}
