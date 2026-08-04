//! `bn review-pr` — the mechanical surface of the idle-triggered auto-review loop. The cron
//! orchestrates; this command owns the ledger and the worktree/favourite actions, so the script
//! layer stays thin:
//!
//!   next   → idle + lock gate, pick the PR to review now (or nothing)
//!   setup  → claim the lock, create the worktree in the PR's own repo, favourite (best-effort), pull
//!   record → store the Approve/Reject recommendation + reviewed sha, release the lock
//!   list   → the tracked PRs and their lifecycle stage (for the poll job + visibility)
//!   remove → drop a PR once it has completed or been abandoned and its worktree was reclaimed
//!   stage  → repaint the ASCII pipeline-stage tracker in the review window (cosmetic/deterministic)
//!
//! The review reasoning itself (fan-out subagents → reference check → verdict) lives in the
//! `auto-review-pr` skill (`config/prompts/auto-review-pr.md`) the spawned agent runs; this command
//! is the plumbing around it, and `stage` is the per-step visual that skill repaints as it walks
//! the pipeline (`get-pr → lease → lint → review → test → fix → verdict`).

use bn_core::autoreview::{self, Stage};
use bn_core::resolver::Config;
use bn_core::reviews::{self, ReviewPr};
use bn_core::util::now_epoch_secs;
use clap::Subcommand;

use crate::wt;

#[derive(Subcommand, Debug)]
pub enum ReviewPrCmd {
    /// The next PR to review now: applies the idle gate and the one-at-a-time lock, then picks
    /// the longest-waiting unreviewed (or force-pushed) review from the cached queue. Prints
    /// nothing when busy, active, or the queue is empty — the cron's "anything to do?" probe.
    Next {
        /// Minutes of quiet required before the loop may start a review. Default 5.
        #[arg(long, default_value_t = 5)]
        idle_mins: i64,
        /// Ignore the idle gate (still honours the lock) — for a manual kick.
        #[arg(long)]
        force: bool,
    },
    /// Claim the lock and prepare the worktree for `<repo> <number>`: resolve the source branch,
    /// create the worktree in the PR's own repo, best-effort favourite the branch (ADO), and pull.
    Setup {
        repo: String,
        number: i64,
        /// Source branch (resolved via az/gh if omitted).
        #[arg(long)]
        branch: Option<String>,
        /// PR url (used if the PR isn't in the cached queue).
        #[arg(long)]
        url: Option<String>,
        /// Skip the favourite step.
        #[arg(long)]
        no_favourite: bool,
        /// Create the worktree + tmux window but don't launch the review agent.
        #[arg(long)]
        no_agent: bool,
        /// Don't touch the ledger / take the in-flight lock — pure mechanical run (for validation).
        #[arg(long)]
        no_lock: bool,
        /// Unattended (cron) mode: prepare the worktree + take the lock but DON'T launch an agent;
        /// instead raise a "ready to review" attention item for you to run the watched review. The
        /// secure auto-trigger — no agent runs on untrusted PR content without a human present.
        #[arg(long)]
        unattended: bool,
    },
    /// Record the recommendation and release the lock: `<verdict>` is approve / reject / needs-work.
    Record {
        repo: String,
        number: i64,
        verdict: String,
        /// Reviewed commit sha (defaults to the worktree's current HEAD).
        #[arg(long)]
        sha: Option<String>,
        /// The review's reasoning/summary — shown in the attention detail (second) view. Pass
        /// `-` to read it from stdin (for long, multi-line write-ups).
        #[arg(long)]
        notes: Option<String>,
    },
    /// The PRs the loop is tracking, with stage + verdict.
    List,
    /// Lifecycle sweep over the tracked PRs (the poll job): reclaim worktrees whose PR has
    /// completed or been abandoned, and re-fetch still-active PRs — flagging a re-review when new
    /// commits landed since the recorded verdict. Hits the network (az/gh + git fetch).
    Poll,
    /// Tear a review down fully: drop it from the ledger AND kill its tmux window + remove the
    /// worktree and local branch. `--keep-worktree` for ledger-only.
    Remove {
        repo: String,
        number: i64,
        /// Branch to tear down, if it isn't in the ledger (e.g. a `--no-lock` run).
        #[arg(long)]
        branch: Option<String>,
        /// Only drop the ledger entry; leave the worktree + tmux window in place.
        #[arg(long)]
        keep_worktree: bool,
    },
    /// Repaint the ASCII pipeline-stage tracker in the review window: the ordered stages with the
    /// one now starting marked. Cosmetic and deterministic — the `auto-review-pr` skill calls it at
    /// the top of each step so the tmux window shows live progress. `--mine` keeps the `fix` stage
    /// (small fixes pushed to your own PR); omit it for a strictly read-only review of another
    /// author's PR.
    Stage {
        /// The stage now starting: get-pr | lease | lint | review | test | fix | verdict.
        stage: String,
        /// This is your own PR — include the fix+push stage in the tracker.
        #[arg(long)]
        mine: bool,
    },
}

/// `jobs/auto-review-trigger.sh`: the cron trigger. When idle, pick the next queued PR and prep it
/// unattended (`review-pr next` → `review-pr setup … --unattended`, folded into one in-process
/// call so there's no TSV round-trip). Returns the job's exit code.
pub fn auto_trigger(cfg: &Config) -> i32 {
    let timer = bn_core::log::enabled().then(bn_core::log::Timer::start);
    let now = now_epoch_secs();
    // Mirror `review-pr next`'s default idle gate (5 min); the one-at-a-time lock is honoured
    // inside `cmd_setup`.
    if !bn_core::idle::is_idle(cfg, now, 5) {
        return 0;
    }
    let cache = reviews::read();
    let led = autoreview::read();
    let Some(pr) = led.pick_next(&cache.prs, now) else {
        return 0;
    };
    println!("auto-review-trigger: preparing {}#{}", pr.repo, pr.number);
    // Same arguments as `review-pr setup <repo> <number> --unattended`: favourite + lock on,
    // agent launch off (unattended forces it off regardless).
    let ok = cmd_setup(
        cfg,
        &pr.repo,
        pr.number,
        None,
        Some(pr.url.clone()),
        true,  // favourite (no_favourite = false)
        true,  // launch_agent (no_agent = false)
        true,  // lock (no_lock = false)
        true,  // unattended
        false, // json
    );
    if ok {
        bn_core::log::record(
            bn_core::log::LogLevel::Info,
            "review-pr",
            "auto_trigger",
            serde_json::json!({ "idle_gate": true }),
            serde_json::json!({ "ok": true }),
            timer.as_ref().map(|t| t.elapsed_ms()),
        );
        0
    } else {
        1
    }
}

pub fn run(cfg: &Config, action: ReviewPrCmd, json: bool) -> bool {
    let timer = bn_core::log::enabled().then(bn_core::log::Timer::start);
    bn_core::log::action(
        "review-pr",
        "route",
        serde_json::json!({ "action": format!("{action:?}"), "json": json }),
    );
    let ok = match action {
        ReviewPrCmd::Next { idle_mins, force } => cmd_next(cfg, idle_mins, force, json),
        ReviewPrCmd::Setup {
            repo,
            number,
            branch,
            url,
            no_favourite,
            no_agent,
            no_lock,
            unattended,
        } => cmd_setup(
            cfg,
            &repo,
            number,
            branch,
            url,
            !no_favourite,
            !no_agent,
            !no_lock,
            unattended,
            json,
        ),
        ReviewPrCmd::Record {
            repo,
            number,
            verdict,
            sha,
            notes,
        } => cmd_record(cfg, &repo, number, &verdict, sha, notes, json),
        ReviewPrCmd::List => cmd_list(json),
        ReviewPrCmd::Poll => cmd_poll(cfg, json),
        ReviewPrCmd::Remove {
            repo,
            number,
            branch,
            keep_worktree,
        } => cmd_remove(cfg, &repo, number, branch, keep_worktree),
        ReviewPrCmd::Stage { stage, mine } => {
            print!("{}", render_stage_tracker(&stage, mine));
            true
        }
    };
    if let Some(timer) = timer {
        bn_core::log::record(
            bn_core::log::LogLevel::Info,
            "review-pr",
            "route_done",
            serde_json::json!({ "json": json }),
            serde_json::json!({ "ok": ok }),
            Some(timer.elapsed_ms()),
        );
    }
    ok
}

fn cmd_remove(
    cfg: &Config,
    repo: &str,
    number: i64,
    branch: Option<String>,
    keep_worktree: bool,
) -> bool {
    // Resolve the branch before dropping the ledger entry (which is one source for it).
    let mut led = autoreview::read();
    let entry_branch = led.find(repo, number).map(|e| e.branch.clone());
    led.remove(repo, number);
    if let Err(e) = autoreview::write(&led) {
        eprintln!("review-pr: failed to write ledger: {e}");
        return false;
    }

    if !keep_worktree {
        match branch.or(entry_branch).or_else(|| wt::resolve_pr_branch(cfg, repo, number)) {
            Some(b) => {
                wt::teardown_review_worktree(cfg, repo, &b);
                println!("removed {repo}#{number} — ledger, tmux window, worktree + branch ({b})");
                return true;
            }
            None => eprintln!(
                "review-pr: couldn't determine the branch to tear down — pass --branch, or --keep-worktree"
            ),
        }
    }
    println!("removed {repo}#{number} (ledger only)");
    true
}

fn cmd_next(cfg: &Config, idle_mins: i64, force: bool, json: bool) -> bool {
    let now = now_epoch_secs();
    if !force && !bn_core::idle::is_idle(cfg, now, idle_mins) {
        if json {
            println!(
                "{}",
                serde_json::json!({"ready": false, "reason": "active"})
            );
        }
        return true;
    }
    let cache = reviews::read();
    let led = autoreview::read();
    match led.pick_next(&cache.prs, now) {
        Some(pr) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "ready": true,
                        "repo": pr.repo,
                        "number": pr.number,
                        "url": pr.url,
                        "title": pr.title,
                    })
                );
            } else {
                // Tab-delimited so the cron can `cut` it: repo<TAB>number<TAB>url
                println!("{}\t{}\t{}", pr.repo, pr.number, pr.url);
            }
            true
        }
        None => {
            if json {
                let reason = if led.is_locked(now) {
                    "locked"
                } else {
                    "empty"
                };
                println!("{}", serde_json::json!({"ready": false, "reason": reason}));
            }
            true
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn cmd_setup(
    cfg: &Config,
    repo: &str,
    number: i64,
    branch: Option<String>,
    url: Option<String>,
    favourite: bool,
    launch_agent: bool,
    lock: bool,
    unattended: bool,
    json: bool,
) -> bool {
    let timer = bn_core::log::enabled().then(bn_core::log::Timer::start);
    // Unattended (cron) mode never launches an agent — it preps + notifies for a watched review.
    let launch_agent = launch_agent && !unattended;
    let now = now_epoch_secs();

    // Prefer the queued PR (carries url + updated_at for force-push dedup); else synthesize one.
    let cache = reviews::read();
    let pr = cache
        .prs
        .iter()
        .find(|p| p.repo == repo && p.number == number)
        .cloned()
        .unwrap_or_else(|| ReviewPr {
            repo: repo.to_string(),
            number,
            title: format!("{repo}#{number}"),
            author: String::new(),
            url: url.unwrap_or_default(),
            updated_at: String::new(),
            created_at: String::new(),
            tag: "review".to_string(),
            state: "open".to_string(),
            review_decision: None,
        });

    let branch = match branch.or_else(|| wt::resolve_pr_branch(cfg, repo, number)) {
        Some(b) => b,
        None => {
            eprintln!("review-pr: could not resolve source branch for {repo}#{number}");
            return false;
        }
    };

    // Claim the lock first (authoritative one-at-a-time gate, races and all). `--no-lock` skips
    // the ledger entirely — a pure mechanical run that can't orphan a lock (used for validation).
    if lock {
        let mut led = autoreview::read();
        if let Err(e) = led.begin(&pr, &branch, now) {
            eprintln!("review-pr: {e}");
            return false;
        }
        if let Err(e) = autoreview::write(&led) {
            eprintln!("review-pr: failed to write ledger: {e}");
            return false;
        }
    }

    // Create the worktree in the PR's own repo. On failure, release the lock so we don't wedge.
    let setup = match wt::setup_review_worktree(cfg, repo, number, &branch, favourite, launch_agent)
    {
        Some(s) => s,
        None => {
            if lock {
                let mut led = autoreview::read();
                led.remove(repo, number);
                let _ = autoreview::write(&led);
            }
            eprintln!("review-pr: worktree setup failed for {repo}#{number}");
            return false;
        }
    };

    // Unattended: no agent ran — raise a "ready to review" attention item so you can run the
    // watched review yourself (the secure auto-trigger surfaces work without an unattended agent).
    if unattended {
        let key = format!("auto-review-ready:{repo}#{number}");
        let title = format!("PR {repo}#{number} ready to review");
        let detail = format!(
            "Worktree prepared at {}. Attach to the {repo} tmux session ({}) and review, then run: \
             bn review-pr record {repo} {number} <approve|reject> --notes \"...\"",
            setup.worktree.display(),
            setup.branch,
        );
        let link = (!pr.url.is_empty()).then_some(pr.url.as_str());
        let _ = bn_core::attention::add_with_detail(
            "auto-review",
            &title,
            "info",
            Some(repo),
            Some(&setup.branch),
            link,
            Some(&key),
            &bn_core::util::today(),
            Some(&detail),
        );
    }
    bn_core::log::record(
        bn_core::log::LogLevel::Info,
        "review-pr",
        "setup",
        serde_json::json!({
            "repo": repo,
            "number": number,
            "branch": setup.branch.as_str(),
            "favourite": favourite,
            "launch_agent": launch_agent,
            "lock": lock,
            "unattended": unattended,
            "json": json,
        }),
        serde_json::json!({
            "worktree": setup.worktree.display().to_string(),
            "remote": setup.remote_label.as_str(),
        }),
        timer.as_ref().map(|t| t.elapsed_ms()),
    );

    if json {
        println!(
            "{}",
            serde_json::json!({
                "repo": repo,
                "number": number,
                "branch": setup.branch,
                "worktree": setup.worktree.to_string_lossy(),
                "remote": setup.remote_label,
                "favourited": setup.favourited,
                "unattended": unattended,
            })
        );
    } else {
        println!("worktree: {}", setup.worktree.display());
        println!("branch:   {} ({})", setup.branch, setup.remote_label);
        println!("favourited: {}", setup.favourited);
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn cmd_record(
    cfg: &Config,
    repo: &str,
    number: i64,
    verdict: &str,
    sha: Option<String>,
    notes: Option<String>,
    json: bool,
) -> bool {
    let timer = bn_core::log::enabled().then(bn_core::log::Timer::start);
    let mut led = autoreview::read();
    let Some(entry) = led.find(repo, number) else {
        eprintln!("review-pr: {repo}#{number} is not tracked");
        return false;
    };
    // `--notes -` reads the (possibly long, multi-line) write-up from stdin.
    let notes = match notes.as_deref() {
        Some("-") => {
            let mut buf = String::new();
            let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf);
            Some(buf.trim().to_string()).filter(|s| !s.is_empty())
        }
        other => other.map(str::to_string).filter(|s| !s.is_empty()),
    };
    let branch = entry.branch.clone();
    let url = entry.url.clone();
    let sha = sha
        .or_else(|| worktree_head(cfg, repo, &branch))
        .unwrap_or_default();
    led.record(repo, number, verdict, &sha);
    if let Err(e) = autoreview::write(&led) {
        eprintln!("review-pr: failed to write ledger: {e}");
        return false;
    }

    // Surface the recommendation in the attention queue (so it shows in `bn next` / notify).
    // Idempotent per PR, and a "reject" recommendation warrants a louder severity than "approve".
    let verdict_norm = verdict.trim().to_lowercase();
    let severity = if verdict_norm.starts_with("approve") {
        "info"
    } else {
        "warn"
    };
    let title = format!(
        "PR review ready: {repo}#{number} -> {}",
        verdict_norm.to_uppercase()
    );
    let link = (!url.is_empty()).then_some(url.as_str());
    let key = format!("auto-review:{repo}#{number}");
    let _ = bn_core::attention::add_with_detail(
        "auto-review",
        &title,
        severity,
        Some(repo),
        Some(&branch),
        link,
        Some(&key),
        &bn_core::util::today(),
        notes.as_deref(),
    );

    // Persist the full judgement to the branch's note folder as a versioned file — review.md, then
    // review2.md, review3.md, … on each re-review when the PR changes. The durable artifact, beyond
    // the ledger + attention.
    let review_file = write_review_note(
        cfg,
        repo,
        &branch,
        number,
        &verdict_norm,
        &sha,
        &url,
        notes.as_deref(),
    );

    if json {
        println!(
            "{}",
            serde_json::json!({
                "repo": repo, "number": number, "verdict": verdict, "sha": sha,
                "review_file": review_file.as_ref().map(|p| p.to_string_lossy()),
            })
        );
    } else {
        println!("recorded {repo}#{number}: {verdict} @ {sha}");
        if let Some(p) = &review_file {
            println!("review note: {}", p.display());
        }
    }
    bn_core::log::record(
        bn_core::log::LogLevel::Info,
        "review-pr",
        "record",
        serde_json::json!({
            "repo": repo,
            "number": number,
            "verdict": verdict,
            "json": json,
        }),
        serde_json::json!({
            "sha": sha,
            "review_file": review_file.as_ref().map(|p| p.to_string_lossy().to_string()),
        }),
        timer.as_ref().map(|t| t.elapsed_ms()),
    );
    true
}

/// Write the review judgement to the branch's note folder as the next versioned `review*.md`
/// (`review.md`, then `review2.md`, `review3.md`, …). Returns the path written, or `None` on any
/// filesystem error.
#[allow(clippy::too_many_arguments)]
fn write_review_note(
    cfg: &Config,
    repo: &str,
    branch: &str,
    number: i64,
    verdict: &str,
    sha: &str,
    url: &str,
    notes: Option<&str>,
) -> Option<std::path::PathBuf> {
    let ctx = bn_core::resolver::Resolved {
        repo: repo.to_string(),
        branch: branch.to_string(),
    };
    let dir = cfg.note_file(&ctx).parent()?.to_path_buf();
    std::fs::create_dir_all(&dir).ok()?;

    // review.md first, then review2.md, review3.md, …
    let path = {
        let first = dir.join("review.md");
        if !first.exists() {
            first
        } else {
            let mut n = 2;
            loop {
                let p = dir.join(format!("review{n}.md"));
                if !p.exists() {
                    break p;
                }
                n += 1;
            }
        }
    };

    let pr_line = if url.is_empty() {
        String::new()
    } else {
        format!("- PR: {url}\n")
    };
    let body = format!(
        "# Review: {repo}#{number} — {}\n\n\
         - Verdict: **{verdict}**\n\
         - Reviewed sha: `{sha}`\n\
         - Branch: `{branch}`\n\
         - Recorded: {today}\n{pr_line}\n\
         ## Findings\n\n{notes}\n",
        verdict.to_uppercase(),
        today = bn_core::util::today(),
        notes = notes.unwrap_or("_(no notes provided)_"),
    );
    std::fs::write(&path, body).ok()?;
    Some(path)
}

fn cmd_list(json: bool) -> bool {
    let led = autoreview::read();
    if json {
        println!("{}", serde_json::to_string(&led).unwrap_or_default());
        return true;
    }
    if led.entries.is_empty() {
        println!("No PRs under review.");
        return true;
    }
    for e in &led.entries {
        let stage = match e.stage {
            Stage::InFlight => "in-flight",
            Stage::Reviewed => "reviewed",
        };
        let verdict = e.verdict.as_deref().unwrap_or("-");
        println!(
            "{}#{}  [{stage}]  {verdict}  {}",
            e.repo, e.number, e.branch
        );
    }
    true
}

fn cmd_poll(cfg: &Config, json: bool) -> bool {
    let today = bn_core::util::today();
    let mut led = autoreview::read();
    let mut reclaimed: Vec<String> = Vec::new();
    let mut rereview: Vec<String> = Vec::new();
    let mut active: Vec<String> = Vec::new();

    for e in led.entries.clone() {
        let tag = format!("{}#{}", e.repo, e.number);
        match wt::review_pr_state(cfg, &e.repo, &e.branch).as_str() {
            // Completed (merged) or abandoned → reclaim: tear the worktree down, drop the ledger
            // entry, and clear the now-moot recommendation from the attention queue.
            s @ ("MERGED" | "CLOSED") => {
                wt::teardown_review_worktree(cfg, &e.repo, &e.branch);
                led.remove(&e.repo, e.number);
                let key = format!("auto-review:{}#{}", e.repo, e.number);
                if let Some(it) = bn_core::attention::open_items(&today)
                    .into_iter()
                    .find(|i| i.key.as_deref() == Some(key.as_str()))
                {
                    let _ = bn_core::attention::ack(&it.id);
                }
                let why = if s == "MERGED" {
                    "completed"
                } else {
                    "abandoned"
                };
                reclaimed.push(format!("{tag} ({why})"));
            }
            // Still active → keep fetching. New commits since the recorded verdict → flag re-review.
            "OPEN" => {
                let tip = wt::refresh_review_branch(cfg, &e.repo, &e.branch);
                let moved = e.stage == Stage::Reviewed
                    && tip.is_some()
                    && e.reviewed_sha.as_deref() != tip.as_deref();
                if moved {
                    let key = format!("auto-review-rereview:{}#{}", e.repo, e.number);
                    let prior = e.verdict.as_deref().unwrap_or("prior");
                    let title =
                        format!("PR {tag} has new commits since your {prior} review — re-review");
                    let link = (!e.url.is_empty()).then_some(e.url.as_str());
                    let _ = bn_core::attention::add(
                        "auto-review",
                        &title,
                        "warn",
                        Some(&e.repo),
                        Some(&e.branch),
                        link,
                        Some(&key),
                        &today,
                    );
                    rereview.push(tag);
                } else {
                    active.push(tag);
                }
            }
            // NONE / unknown → likely a transient API miss; leave it for the next sweep.
            _ => active.push(tag),
        }
    }

    if let Err(e) = autoreview::write(&led) {
        eprintln!("review-pr: failed to write ledger: {e}");
        return false;
    }

    if json {
        println!(
            "{}",
            serde_json::json!({
                "reclaimed": reclaimed,
                "rereview": rereview,
                "active": active,
            })
        );
    } else {
        println!(
            "poll: {} reclaimed, {} flagged for re-review, {} still active",
            reclaimed.len(),
            rereview.len(),
            active.len()
        );
        for r in &reclaimed {
            println!("  reclaimed {r}");
        }
        for r in &rereview {
            println!("  re-review {r}");
        }
    }
    true
}

/// Current HEAD sha of the review worktree for `<repo>`'s `branch`, if it exists. Empty output
/// (no such worktree / detached with no HEAD) maps to `None`, so the trimmed `git_capture` result
/// is filtered for non-emptiness.
fn worktree_head(cfg: &Config, repo: &str, branch: &str) -> Option<String> {
    let wt_path = wt::review_worktree_path(cfg, repo, branch);
    crate::git::git_capture(&["rev-parse", "HEAD"], Some(&wt_path), true).filter(|s| !s.is_empty())
}

/// The ordered PR-review pipeline stages, each as `(key, one-line what-happens)`. The `fix` stage
/// only applies to *your own* PR (small fixes pushed to the branch); a review of someone else's PR
/// is strictly read-only and skips straight from `test` to `verdict`, so [`render_stage_tracker`]
/// filters `fix` out unless `mine`.
const REVIEW_STAGES: &[(&str, &str)] = &[
    ("get-pr", "read the diff, state what it claims"),
    ("lease", "worktree checked out on the PR branch"),
    ("lint", "format / lint gate"),
    (
        "review",
        "reference-aware review + pr-review-toolkit subagents",
    ),
    ("test", "targeted tests (when code changed)"),
    ("fix", "autofix small issues + push  [your own PR only]"),
    ("verdict", "record the recommendation"),
];

/// Render the ASCII pipeline-stage tracker for the review agent's tmux window: every stage with a
/// marker — done `[x]`, current `[>]`, pending `[ ]`. `mine` keeps the `fix` stage (your own PR
/// gets small fixes pushed); a review of another author's PR drops it (strictly read-only). Pure
/// and deterministic so it is unit-testable and safe to repaint on every step.
fn render_stage_tracker(current: &str, mine: bool) -> String {
    let stages: Vec<&(&str, &str)> = REVIEW_STAGES
        .iter()
        .filter(|(key, _)| mine || *key != "fix")
        .collect();
    let cur_idx = stages.iter().position(|(key, _)| *key == current);
    let mode = if mine {
        "mine — fix small issues + push"
    } else {
        "review — strictly read-only"
    };
    let mut out = format!("PR review pipeline  [{mode}]\n");
    for (i, (key, blurb)) in stages.iter().enumerate() {
        let marker = match cur_idx {
            Some(c) if i < c => "[x]",
            Some(c) if i == c => "[>]",
            _ => "[ ]",
        };
        out.push_str(&format!("  {marker} {key:<8} {blurb}\n"));
    }
    out
}


