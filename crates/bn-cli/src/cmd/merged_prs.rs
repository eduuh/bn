//! `bn merged-prs gather` — the day's own merged PRs, with the material needed to talk about them.
//!
//! The I/O edge over [`bn_core::merged_prs`]: discover the merges offline from the metrics log,
//! fetch each PR's diff and metadata through `gh`, read the branch note for the *why*, then order
//! largest-first and cap. It reviews nothing and lands nothing — the `staff-mentor` agent's prompt
//! owns the judgment, and this command is the seam between them.

use crate::*;
use bn_core::merged_prs as merged;
use bn_core::runner::CommandRunner;

/// Per-PR diff cap (bytes): three capped diffs stay well under the staff framework's
/// untrusted-packet size limit, and a giant refactor's full diff teaches no more than its first
/// tens-of-KB. Overflow past this is marked in the packet so the reader knows it saw a prefix.
const DIFF_CAP_BYTES: usize = 40_000;

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
) -> Option<merged::PrPacket> {
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
    Some(merged::PrPacket {
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
        repo: merged::bare_repo(repo_slug),
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
pub(crate) fn gather_merged_prs(
    cfg: &Config,
    runner: &dyn CommandRunner,
    today: &str,
) -> merged::MergedPrPacket {
    let reviewed_date = bn_core::util::add_days(today, -1).unwrap_or_else(|| today.to_string());
    let events = bn_core::events::read_events_across(&cfg.metrics_logs_all_machines());
    let merged = merged::discover_merged(&events, &reviewed_date);
    let prs: Vec<merged::PrPacket> = merged
        .iter()
        .filter_map(|m| fetch_pr(cfg, runner, &m.repo, m.number))
        .collect();
    merged::build_packet(today, &reviewed_date, prs, merged::DEFAULT_PR_CAP)
}

/// `bn merged-prs gather` — print the packet as JSON. Exits 0 on a printed packet; a day that
/// merged nothing is a legitimately empty packet, and a reader that is a staff agent declines the
/// run rather than dispatching a model over it.
pub(crate) fn emit_merged_prs_gather(cfg: &Config) -> i32 {
    let packet = gather_merged_prs(cfg, &bn_core::runner::RealRunner, &today());
    if packet.prs.is_empty() {
        println!(
            "{}",
            serde_json::json!({
                "skip": format!("no PRs of yours merged on {}", packet.reviewed_date)
            })
        );
        return 0;
    }
    match packet.to_json() {
        Ok(json) => {
            println!("{json}");
            0
        }
        Err(e) => {
            eprintln!("bn merged-prs gather: serialize packet: {e}");
            1
        }
    }
}
