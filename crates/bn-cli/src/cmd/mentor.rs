//! `bn mentor` + the `staff-mentor` cron (#258) — the Mentor's IO shell around the pure
//! [`bn_core::mentor`] core.
//!
//! The deterministic half (discover yesterday's merged own-PRs offline from the metrics log, fetch
//! their diffs + branch-note context, cap largest-first) and the routing half (append an
//! idempotent craft note to each merged branch's note, reinforce a dojo deck card per theme behind
//! the `[dojo]` feature check, write one staff briefing card to the feed) live here; the pure
//! contract validation, theme slugs, and card builders live in [`bn_core::mentor`]. The headless
//! review dispatch reuses the staff framework's harness plumbing ([`staff_dispatch_command`] /
//! [`dispatch_capture`] / [`bn_core::staff::classify_run`]) so auth / rate-limited runs are
//! classified exactly like every other agent.

use crate::*;
use bn_core::mentor;
use bn_core::runner::CommandRunner;
use bn_core::staff::{self, Dispatched, RunStatus};
use std::path::PathBuf;
use std::time::Duration;

/// Per-PR diff cap (bytes) put on the packet: three capped diffs stay well under the staff
/// framework's untrusted-packet size limit, and a giant refactor's full diff teaches no more than
/// its first tens-of-KB. Overflow past this is marked in the packet so the model knows it saw a
/// prefix.
const DIFF_CAP_BYTES: usize = 40_000;

/// Wall-clock budget for one review dispatch — a strong-model review of a capped diff, killed past
/// this so a wedged harness can't hang the cron.
const WALL_SECS: u64 = 600;

/// What one Mentor run did — the summary `bn mentor` and the cron print, and the shape tests
/// assert against.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MentorReport {
    pub ok: bool,
    pub status: RunStatus,
    pub error: Option<String>,
    /// PRs reviewed this run (0 on a zero-merge day or a failure).
    pub reviewed: usize,
    pub total_merged: usize,
    pub overflow: usize,
    /// Craft notes appended this run (a re-run appends none — already present / not re-routed).
    pub craft_notes: usize,
    /// Dojo deck cards added this run (0 when the `[dojo]` feature is off — feed-only).
    pub dojo_cards: usize,
    pub card_path: Option<PathBuf>,
    /// True on a zero-merge day: no dispatch, no card, no event — silence, not filler.
    pub empty: bool,
    pub dojo_enabled: bool,
}

// --- gather (offline discovery + network diff fetch) ---------------------------------------------

/// The metadata `gh pr view` yields for one PR — title/body (untrusted external content),
/// head branch (the craft-note target), and the html url.
#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PrMeta {
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    head_ref_name: String,
    #[serde(default)]
    url: String,
}

/// Fetch one PR's review material via `gh` (through the runner seam, so tests drive it with
/// [`bn_core::runner::FakeRunner`]). `None` when the metadata fetch fails (gh absent / errored) —
/// that PR is skipped, never fabricated. The diff is capped ([`DIFF_CAP_BYTES`]) with the cut
/// noted; `diff_bytes` keeps the *full* size so the largest-first ordering stays honest.
fn fetch_pr(
    cfg: &Config,
    runner: &dyn CommandRunner,
    repo: &str,
    number: i64,
) -> Option<mentor::PrPacket> {
    let num = number.to_string();
    let meta: PrMeta = bn_core::runner::gh_json(
        runner,
        &[
            "pr",
            "view",
            &num,
            "--repo",
            repo,
            "--json",
            "title,body,headRefName,url",
        ],
    )?;
    let full_diff = runner
        .gh(&["pr", "diff", &num, "--repo", repo])
        .ok_stdout()
        .unwrap_or_default();
    let diff_bytes = full_diff.len();
    let diff = if diff_bytes > DIFF_CAP_BYTES {
        let mut d: String = full_diff.chars().take(DIFF_CAP_BYTES).collect();
        d.push_str("\n… [diff truncated for review]\n");
        d
    } else {
        full_diff
    };
    let (goal, decisions, verify_history) = branch_note_context(cfg, repo, &meta.head_ref_name);
    Some(mentor::PrPacket {
        repo: repo.to_string(),
        number,
        branch: meta.head_ref_name,
        title: meta.title,
        url: meta.url,
        description: meta.body,
        diff,
        diff_bytes,
        goal,
        decisions,
        verify_history,
    })
}

/// Read the merged branch's note (on this machine) for the "why" a post-hoc reviewer never has:
/// its one-line goal and its decisions. Best-effort — a note that never lived on this machine
/// (a PR merged elsewhere) degrades to empty context, it never errors. Verify-gate history has no
/// per-branch record yet (depends on unbuilt plumbing), so it's returned empty for now.
fn branch_note_context(
    cfg: &Config,
    repo_slug: &str,
    branch: &str,
) -> (Option<String>, Vec<String>, Vec<String>) {
    if branch.is_empty() {
        return (None, Vec::new(), Vec::new());
    }
    let ctx = Resolved {
        repo: mentor::bare_repo(repo_slug),
        branch: branch.to_string(),
    };
    let Ok(note) = bn_core::store::read(&cfg.note_file(&ctx)) else {
        return (None, Vec::new(), Vec::new());
    };
    let goal = bn_core::render::section_content(&note.body, "## Goal")
        .into_iter()
        .find(|l| !l.trim().is_empty());
    let decisions = note
        .front
        .decisions
        .iter()
        .map(|i| i.text.clone())
        .collect();
    (goal, decisions, Vec::new())
}

/// Gather the deterministic packet: discover *your* PRs merged yesterday offline from the metrics
/// log, fetch each one's diff + context, then cap largest-first. `runner` is the `gh` seam.
pub(crate) fn gather_mentor_packet(
    cfg: &Config,
    runner: &dyn CommandRunner,
    today: &str,
) -> mentor::MentorPacket {
    let reviewed_date = bn_core::util::add_days(today, -1).unwrap_or_else(|| today.to_string());
    let events = bn_core::events::read_events_across(&cfg.metrics_logs_all_machines());
    let merged = mentor::discover_merged(&events, &reviewed_date);
    let prs: Vec<mentor::PrPacket> = merged
        .iter()
        .filter_map(|m| fetch_pr(cfg, runner, &m.repo, m.number))
        .collect();
    mentor::build_packet(today, &reviewed_date, prs, mentor::DEFAULT_PR_CAP)
}

// --- dispatch prompt -----------------------------------------------------------------------------

/// The full review dispatch prompt: the pinned skill, the packet inside the untrusted-data frame
/// ([`staff::frame_packet`], the shared #272 guard), then the four-section output contract with the
/// exact JSON the harness must return.
fn strip_frontmatter(text: &str) -> String {
    if let Some(rest) = text.strip_prefix("---") {
        if let Some((_, body)) = rest.split_once("---") {
            return body.trim_start().to_string();
        }
    }
    text.to_string()
}

fn mentor_skill_body() -> String {
    strip_frontmatter(include_str!("../../../../config/skills/staff/mentor.md"))
}

fn mentor_prompt(skill_body: &str, packet_json: &str, today: &str) -> String {
    format!(
        "{skill_body}\n\n{framed}\n\n\
         For EACH pr in the packet, output one review satisfying the four-section contract. Output \
         EXACTLY one JSON object and nothing else:\n\
         {{ \"headline\": \"...\", \"reviews\": [ {{ \"repo\": \"owner/repo\", \"number\": 0, \
         \"stronger_design\": \"the approach a staff engineer might have taken — or 'this was the \
         right shape' when none genuinely exists (never empty)\", \
         \"idiom\": \"one concrete technique worth stealing, with a code sketch\", \
         \"strength\": \"one genuine strength specific to this diff\", \
         \"theme\": \"a short recurring-theme tag, e.g. error-paths\" }} ] }}\n\
         Date: {today}. Never post anything anywhere — these are private notes.\n",
        framed = staff::frame_packet(packet_json),
    )
}

// --- routing (idempotent craft note + dojo card) -------------------------------------------------

/// Append the craft note to the merged branch's note — once. Idempotent: only when the note
/// already exists on this machine ("closed notes accept appends") and doesn't already carry this
/// PR's [`mentor::craft_marker`]. Returns whether a note was newly appended.
fn append_craft_note(cfg: &Config, r: &mentor::PrReview, today: &str) -> bool {
    let ctx = Resolved {
        repo: mentor::bare_repo(&r.repo),
        branch: r.branch.clone(),
    };
    if r.branch.is_empty() || !cfg.note_file(&ctx).exists() {
        return false; // no note on this machine — nothing to append to
    }
    let marker = mentor::craft_marker(&r.repo, r.number);
    let existing = bn_core::commands::read_body_section(cfg, &ctx, "notes").unwrap_or_default();
    if mentor::note_has_craft(&existing, &marker) {
        return false; // already reviewed on a prior run
    }
    bn_core::commands::append_body(cfg, &ctx, "notes", &mentor::craft_note(r), today).is_ok()
}

/// Reinforce the theme's dojo deck card. Idempotent via the card's stable id (the same theme in the
/// same repo is one card). Returns whether a *new* card was added.
fn add_dojo_card(cfg: &Config, r: &mentor::PrReview) -> bool {
    let card = mentor::dojo_card(r, bn_core::util::now_epoch_secs());
    bn_core::dojo::DojoStore::from_config(cfg)
        .append_cards(&[card])
        .map(|added| added > 0)
        .unwrap_or(false)
}

// --- the pipeline --------------------------------------------------------------------------------

/// Run the Mentor end to end with the dispatch step injected (the CLI/cron pass the real harness;
/// tests pass a closure). Gather → (zero-merge → silence) → dispatch → validate the four-section
/// contract → write the feed card → route (idempotent craft notes + dojo cards, the deck behind
/// `dojo_enabled`). Every non-silent exit logs a `staff_run` event so the Mentor lands on the
/// health board + cost ledger; a genuine `fail` also raises one keyed warn attention item.
pub(crate) fn run_mentor_pipeline(
    cfg: &Config,
    runner: &dyn CommandRunner,
    today: &str,
    dojo_enabled: bool,
    dispatch: impl FnOnce(&str) -> Dispatched,
) -> MentorReport {
    let started = std::time::Instant::now();
    let packet = gather_mentor_packet(cfg, runner, today);

    // Zero-merge day → silence: no dispatch, no card, no event.
    if packet.prs.is_empty() {
        return MentorReport {
            ok: true,
            status: RunStatus::Ok,
            error: None,
            reviewed: 0,
            total_merged: packet.total_merged,
            overflow: packet.overflow,
            craft_notes: 0,
            dojo_cards: 0,
            card_path: None,
            empty: true,
            dojo_enabled,
        };
    }

    let report_fail = |status: RunStatus, error: String, tokens: u64| -> MentorReport {
        let wall_ms = started.elapsed().as_millis() as u64;
        // Only a genuine `fail` pages; auth / rate-limited are surfaced via the health board.
        if status == RunStatus::Fail {
            let key = format!("staff-fail:{}:{today}", mentor::AGENT);
            bn_core::util::best_effort(
                bn_core::attention::add(
                    &staff::job_name(mentor::AGENT),
                    &format!("staff agent '{}' failed: {error}", mentor::AGENT),
                    "warn",
                    None,
                    None,
                    None,
                    Some(&key),
                    today,
                ),
                "mentor: page attention for failure",
            );
        }
        staff::record_run(mentor::AGENT, today, status, Some(&error), tokens, wall_ms);
        MentorReport {
            ok: false,
            status,
            error: Some(error),
            reviewed: 0,
            total_merged: packet.total_merged,
            overflow: packet.overflow,
            craft_notes: 0,
            dojo_cards: 0,
            card_path: None,
            empty: false,
            dojo_enabled,
        }
    };

    let skill = mentor_skill_body();
    let packet_json = match packet.to_json() {
        Ok(json) => json,
        Err(e) => {
            return report_fail(
                RunStatus::Fail,
                format!("packet serialization failed: {e}"),
                0,
            )
        }
    };
    let prompt = mentor_prompt(&skill, &packet_json, today);
    let output = match dispatch(&prompt) {
        Dispatched::Output(o) => o,
        Dispatched::Failed(status, detail) => {
            let status_str = status.as_str();
            let error = if detail.is_empty() {
                format!("dispatch failed ({status_str})")
            } else {
                format!("dispatch failed ({status_str}): {detail}")
            };
            return report_fail(status, error, 0);
        }
    };
    let tokens = staff::estimate_tokens(prompt.len() + output.len());

    let reviews = match mentor::validate_output(&output, &packet) {
        Ok(r) => r,
        Err(e) => return report_fail(RunStatus::Fail, format!("invalid review: {e}"), tokens),
    };

    let card = mentor::feed_card(&packet, &reviews);
    // Same-day re-run: refresh the card but don't re-route (the craft-note marker + dojo id make
    // re-routing idempotent anyway; this mirrors the staff pipeline's rerun guard).
    let rerun = staff::card_path(cfg, &card.date, mentor::AGENT).is_file();
    let card_path = match staff::write_card(cfg, &card) {
        Ok(p) => p,
        Err(e) => return report_fail(RunStatus::Fail, format!("feed write failed: {e}"), tokens),
    };

    let (mut craft_notes, mut dojo_cards) = (0, 0);
    if !rerun {
        for r in &reviews {
            if append_craft_note(cfg, r, today) {
                craft_notes += 1;
            }
            if dojo_enabled && add_dojo_card(cfg, r) {
                dojo_cards += 1;
            }
        }
    }

    let wall_ms = started.elapsed().as_millis() as u64;
    staff::record_run(mentor::AGENT, today, RunStatus::Ok, None, tokens, wall_ms);
    MentorReport {
        ok: true,
        status: RunStatus::Ok,
        error: None,
        reviewed: reviews.len(),
        total_merged: packet.total_merged,
        overflow: packet.overflow,
        craft_notes,
        dojo_cards,
        card_path: Some(card_path),
        empty: false,
        dojo_enabled,
    }
}

/// The real headless review dispatch: a fresh `-p` turn on the copilot harness (the Mentor's
/// default), killed past [`WALL_SECS`], classified via the shared #271 taxonomy.
fn real_dispatch(prompt: &str) -> Dispatched {
    let Some(cmd) = staff_dispatch_command("copilot", prompt, None, None) else {
        return Dispatched::Failed(RunStatus::Fail, "unknown harness 'copilot'".into());
    };
    let (success, stdout, combined) = dispatch_capture(cmd, Some(Duration::from_secs(WALL_SECS)));
    match staff::classify_run(success, &combined) {
        RunStatus::Ok => Dispatched::Output(stdout),
        other => Dispatched::Failed(other, staff::failure_detail(&combined)),
    }
}

/// Print a Mentor run's outcome.
fn print_report(r: &MentorReport) {
    if r.empty {
        println!("mentor: no PRs of yours merged yesterday — nothing to review (silence)");
        return;
    }
    if !r.ok {
        eprintln!(
            "mentor: {} [{}]",
            r.error.as_deref().unwrap_or("failed"),
            r.status.as_str()
        );
        return;
    }
    println!(
        "mentor: reviewed {} PR(s) ({} merged, {} over cap) → craft notes {} · dojo cards {}{}",
        r.reviewed,
        r.total_merged,
        r.overflow,
        r.craft_notes,
        r.dojo_cards,
        if r.dojo_enabled {
            ""
        } else {
            " (dojo off — feed-only)"
        }
    );
    if let Some(p) = &r.card_path {
        println!("  {}", p.display());
    }
}

// --- entry points --------------------------------------------------------------------------------

/// `bn mentor` — run the Mentor now against yesterday's merges and print the card. Always runs
/// (unlike the cron, which gates on `[mentor].enabled`). `--json` emits the full report.
pub(crate) fn emit_mentor(cfg: &Config, as_json: bool) -> i32 {
    let dojo_enabled = resolver::JobConfig::load().dojo_enabled;
    let report = run_mentor_pipeline(
        cfg,
        &bn_core::runner::RealRunner,
        &today(),
        dojo_enabled,
        real_dispatch,
    );
    if as_json {
        println!(
            "{}",
            serde_json::json!({
                "agent": "mentor",
                "ok": report.ok,
                "status": report.status.as_str(),
                "error": report.error,
                "reviewed": report.reviewed,
                "total_merged": report.total_merged,
                "overflow": report.overflow,
                "craft_notes": report.craft_notes,
                "dojo_cards": report.dojo_cards,
                "dojo_enabled": report.dojo_enabled,
                "empty": report.empty,
                "card_path": report.card_path.as_ref().map(|p| p.display().to_string()),
            })
        );
    } else {
        print_report(&report);
    }
    if report.ok {
        0
    } else {
        1
    }
}

/// The `staff-mentor` cron — the Mon..Fri 06:30 post-merge review. Opt-in via `[mentor].enabled`
/// (it dispatches an LLM harness and appends to notes, so it stays off until you turn it on); a
/// manual `bn mentor` runs regardless. Always exits 0 — a declined gate isn't a failure.
pub(crate) fn job_staff_mentor(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let jc = resolver::JobConfig::load();
    if !jc.mentor_enabled {
        println!("staff-mentor: Mentor disabled — set [mentor].enabled to opt in");
        return 0;
    }
    let report = run_mentor_pipeline(
        cfg,
        &bn_core::runner::RealRunner,
        &today(),
        jc.dojo_enabled,
        real_dispatch,
    );
    print_report(&report);
    // A failed dispatch already logged + paged; still report a non-zero exit for cron visibility.
    if report.ok {
        0
    } else {
        1
    }
}


