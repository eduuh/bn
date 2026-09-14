#![allow(
    clippy::panic_in_result_fn,
    reason = "these tests return Result so `?` works on the harness, and assert! is how a \
test reports a failed expectation; the workspace-wide deny targets production code paths"
)]

//! The `bn tat` picker's hidden stages, run the way fzf runs them, against a sandboxed layout —
//! and **timed**, against a committed baseline.
//!
//! The picker is self-referencing: fzf's `start` reload, `--preview` and every key binding
//! re-invoke `bn tat --pick-stage <s>`. The preview runs once per cursor move, so a regression
//! there is felt on every keystroke — which is why this lane measures as well as asserts. Each
//! stage is run three times and the best time kept (the min filters scheduler noise far better
//! than a mean), then compared with `tat-timing.baseline` beside this crate's `Cargo.toml`:
//!
//! * over `baseline × TOLERANCE` (never below `FLOOR_MS`, so a 30ms stage on a loaded runner is
//!   not a failure) → the lane fails and names the stage;
//! * a measured stage with no baseline line, or a baseline line for a stage no longer measured →
//!   the lane fails, so the file stays honest;
//! * `BN_E2E_TIMING_UPDATE=1` rewrites the baseline from this run. Commit the diff when a stage
//!   legitimately changed cost, the way `.file-size-baseline` is regenerated after a split.
//!
//! The baseline is recorded with a **debug** build (what the CI lane runs); a release binary is
//! faster and always passes. What is *not* here: the interactive path through fzf itself (keys,
//! the ctrl-o menu) — `scripts/try-tat.sh` drives that on a real machine — and the two
//! stages that block on a forge by design (`refresh`, `warm-remote`).
//!
//! Run with: `cargo test -p bn-e2e --test tat_picker -- --ignored --test-threads=1`

use anyhow::{bail, Context};
use bn_e2e::{proof, Run, Sandbox};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

macro_rules! require_tmux {
    () => {
        if !proof::tmux_available() {
            eprintln!("skipping: tmux is not installed");
            return Ok(());
        }
    };
}

/// Runs per stage; the fastest is the measurement.
const REPS: usize = 3;
/// How far over its baseline a stage may drift before the lane fails.
const TOLERANCE: f64 = 2.0;
/// No budget is ever tighter than this. The stages run in 20–50ms here, and a 30ms process on a
/// loaded self-hosted runner can honestly take twice that; what the gate is for is the 145-second
/// `ls-files` and the 86-a-minute rediscovery, not scheduler jitter. Four times a stage's cost is
/// the line.
const FLOOR_MS: u128 = 120;

struct Timings(Vec<(String, u128)>);

impl Timings {
    /// Run one stage `REPS` times and keep the fastest, returning that run's output.
    fn stage(&mut self, sb: &Sandbox, name: &str, args: &[&str]) -> anyhow::Result<Run> {
        let mut best: Option<(u128, Run)> = None;
        for _ in 0..REPS {
            let started = Instant::now();
            let run = sb.bn(args)?;
            let ms = started.elapsed().as_millis();
            if best.as_ref().is_none_or(|(b, _)| ms < *b) {
                best = Some((ms, run));
            }
        }
        let (ms, run) = best.context("REPS is at least one")?;
        self.0.push((name.to_string(), ms));
        Ok(run)
    }
}

fn baseline_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tat-timing.baseline")
}

fn read_baseline(path: &Path) -> BTreeMap<String, u128> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let (stage, ms) = l.split_once('\t')?;
            Some((stage.trim().to_string(), ms.trim().parse().ok()?))
        })
        .collect()
}

fn write_baseline(path: &Path, timings: &Timings) -> anyhow::Result<()> {
    let mut out = String::from(
        "# `bn tat` stage timings, in milliseconds — the best of three runs of a debug build in the\n\
         # e2e sandbox (crates/bn-e2e/tests/tat_picker.rs). The lane fails when a stage exceeds\n\
         # its line × 2 (floor 120ms). Regenerate after a deliberate cost change and commit:\n\
         #   BN_E2E_TIMING_UPDATE=1 cargo test -p bn-e2e --test tat_picker -- --ignored --test-threads=1\n",
    );
    let mut rows: Vec<&(String, u128)> = timings.0.iter().collect();
    rows.sort();
    for (stage, ms) in rows {
        // Rounded up to the next 10ms so a rerun on the same machine does not churn the file.
        out.push_str(&format!("{stage}\t{}\n", ms.div_ceil(10) * 10));
    }
    std::fs::write(path, out).with_context(|| format!("write {}", path.display()))
}

/// Compare this run with the baseline, or rewrite it under `BN_E2E_TIMING_UPDATE=1`.
fn check_timings(timings: &Timings) -> anyhow::Result<()> {
    let path = baseline_path();
    let baseline = read_baseline(&path);
    let mut failures = Vec::new();
    eprintln!("{:<26} {:>8} {:>10}", "stage", "ms", "budget");
    for (stage, ms) in &timings.0 {
        match baseline.get(stage) {
            Some(base) => {
                let budget = ((*base as f64) * TOLERANCE).ceil().max(FLOOR_MS as f64) as u128;
                let mark = if *ms > budget { "  SLOW" } else { "" };
                eprintln!("{stage:<26} {ms:>8} {budget:>10}{mark}");
                if *ms > budget {
                    failures.push(format!(
                        "{stage}: {ms}ms exceeds its budget of {budget}ms (baseline {base}ms × {TOLERANCE})"
                    ));
                }
            }
            None => {
                eprintln!("{stage:<26} {ms:>8} {:>10}", "none");
                failures.push(format!("{stage}: not in {}", path.display()));
            }
        }
    }
    for stage in baseline.keys() {
        if !timings.0.iter().any(|(s, _)| s == stage) {
            failures.push(format!("{stage}: in the baseline but no longer measured"));
        }
    }
    if std::env::var("BN_E2E_TIMING_UPDATE").is_ok_and(|v| v == "1") {
        write_baseline(&path, timings)?;
        eprintln!("rewrote {}", path.display());
        return Ok(());
    }
    if !failures.is_empty() {
        bail!(
            "tat stage timing gate failed:\n  {}\n(rerun with BN_E2E_TIMING_UPDATE=1 to accept a deliberate change)",
            failures.join("\n  ")
        );
    }
    Ok(())
}

/// A sandbox with the bare+worktree layout `tat` discovers: one bare repo, one idle worktree, and
/// a tmux session for the repo so the sub-picker has a window row to paint.
fn picker_sandbox() -> anyhow::Result<(Sandbox, PathBuf, PathBuf)> {
    let sb = Sandbox::new()?;
    let bare = sb.seed_bare_repo("repo")?;
    let worktree = sb.path("projects/worktree/repo/feature-a");
    std::fs::create_dir_all(sb.path("projects/worktree/repo"))?;
    sb.git_in(
        sb.root(),
        [
            "--git-dir",
            &bare.to_string_lossy(),
            "worktree",
            "add",
            "-q",
            "-b",
            "feature-a",
            &worktree.to_string_lossy(),
            "main",
        ],
    )?
    .expect_success()?;
    sb.tmux(&[
        "new-session",
        "-d",
        "-s",
        "repo",
        "-c",
        &worktree.to_string_lossy(),
    ])?;
    Ok((sb, bare, worktree))
}

fn assert_says(run: &Run, what: &str) -> anyhow::Result<()> {
    if run.code != Some(0) {
        bail!("{} exited {:?}:\n{}", run.label, run.code, run.output());
    }
    if !run.output().contains(what) {
        bail!("{} should say {what:?}, got:\n{}", run.label, run.output());
    }
    Ok(())
}

/// Every read-only and guarded stage answers the way fzf expects, and within its time budget.
#[test]
#[ignore = "e2e: needs a built bn binary (cargo build --workspace)"]
fn tat_stages_answer_and_stay_within_budget() -> anyhow::Result<()> {
    require_tmux!();
    let (sb, _bare, _worktree) = picker_sandbox()?;
    sb.tmux(&["new-session", "-d", "-s", "zz-kill", "-c", "/tmp"])?;
    let mut t = Timings(Vec::new());

    // The project list: the repo, marked with its live session.
    let list = t.stage(&sb, "list", &["tat", "--pick-stage", "list"])?;
    assert_says(&list, "\trepo")?;
    assert!(
        list.stdout.lines().any(|l| l.starts_with("[*1w] repo")),
        "the repo row should carry its one-window session marker, got:\n{}",
        list.stdout
    );
    let preview = t.stage(&sb, "preview", &["tat", "repo", "--pick-stage", "preview"])?;
    assert_says(&preview, "━━━ repo ━━━")?;
    let missing = t.stage(
        &sb,
        "preview-missing",
        &["tat", "zz-none", "--pick-stage", "preview"],
    )?;
    assert_says(&missing, "Directory not found")?;

    // The sub-picker feed: a window row, the worktree, and the command section.
    let sublist = t.stage(&sb, "sublist", &["tat", "repo", "--pick-stage", "sublist"])?;
    for needle in [
        "window|",
        "worktree|feature-a|",
        "cmdsep||",
        "cmd|build-main|",
    ] {
        assert_says(&sublist, needle)?;
    }
    // The sandbox server has no user config, so its first window is whatever `base-index` tmux
    // defaults to; take the index from the feed rather than assuming a person's `1`.
    let window_arg = sublist
        .stdout
        .lines()
        .find_map(|l| l.strip_prefix("window|"))
        .and_then(|rest| rest.split('|').next())
        .map(|index| format!("window~{index}"))
        .context("the feed should paint a window row")?;
    let subtick = t.stage(&sb, "subtick", &["tat", "repo", "--pick-stage", "subtick"])?;
    assert_says(&subtick, "worktree|feature-a|")?;

    // The side pane, once per row kind the feed painted — what fzf does on every cursor move.
    let mut seen = BTreeMap::new();
    for row in sublist.stdout.lines() {
        let mut parts = row.splitn(3, '|');
        let (kind, payload) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        if kind.is_empty() || seen.contains_key(kind) {
            continue;
        }
        seen.insert(kind.to_string(), ());
        let arg = format!("{kind}~{payload}");
        let run = t.stage(
            &sb,
            &format!("subpreview-{kind}"),
            &[
                "tat",
                "repo",
                "--pick-stage",
                "subpreview",
                "--pick-arg",
                &arg,
            ],
        )?;
        run.expect_success()?;
    }
    for kind in ["window", "worktree", "cmd", "cmdsep"] {
        assert!(
            seen.contains_key(kind),
            "the feed should paint a {kind} row"
        );
    }

    // Guards: every binding fires on whatever is focused, so every stage refuses the wrong row.
    let refused = t.stage(
        &sb,
        "rmworktree-refused",
        &[
            "tat",
            "repo",
            "--pick-stage",
            "rmworktree",
            "--pick-arg",
            &window_arg,
        ],
    )?;
    assert_says(&refused, "window row can't be removed")?;
    let main = sb.bn([
        "tat",
        "repo",
        "--pick-stage",
        "rmworktree",
        "--pick-arg",
        "worktree~main",
    ])?;
    assert_says(&main, "main checkout")?;
    let gone = sb.bn([
        "tat",
        "repo",
        "--pick-stage",
        "rmworktree",
        "--pick-arg",
        "worktree~zz",
    ])?;
    assert_says(&gone, "already gone")?;
    let rel = sb.bn([
        "tat",
        "repo",
        "--pick-stage",
        "releasepool",
        "--pick-arg",
        "worktree~x",
    ])?;
    assert_says(&rel, "focus a leased")?;
    let rel = sb.bn([
        "tat",
        "repo",
        "--pick-stage",
        "releasepool",
        "--pick-arg",
        "poolbranch~zz",
    ])?;
    assert_says(&rel, "no leased slot")?;
    let cmd = sb.bn([
        "tat",
        "repo",
        "--pick-stage",
        "runcmd",
        "--pick-arg",
        &window_arg,
    ])?;
    assert_says(&cmd, "act on a command row")?;
    let cmd = sb.bn([
        "tat",
        "repo",
        "--pick-stage",
        "runcmd",
        "--pick-arg",
        "cmdsep~",
    ])?;
    assert_says(&cmd, "separator")?;
    let cmd = sb.bn([
        "tat",
        "repo",
        "--pick-stage",
        "runcmd",
        "--pick-arg",
        "cmd~zz",
    ])?;
    assert_says(&cmd, "no such command")?;
    let warm = sb.bn(["tat", "repo@win", "--pick-stage", "warmpool"])?;
    assert_says(&warm, "cold path")?;

    // A direct open of a name that is no project says so and creates nothing.
    let unknown = t.stage(&sb, "open-unknown", &["tat", "zz-none"])?;
    assert_says(&unknown, "No such project")?;
    assert!(
        !sb.tmux_sessions()?.iter().any(|s| s == "zz-none"),
        "an unknown name must not create a session"
    );

    // ctrl-x: the throwaway session dies; a name with no session is a no-op.
    t.stage(&sb, "kill", &["tat", "zz-kill", "--pick-stage", "kill"])?
        .expect_success()?;
    assert!(
        !sb.tmux_sessions()?.iter().any(|s| s == "zz-kill"),
        "kill should remove the focused row's session"
    );
    t.stage(
        &sb,
        "kill-missing",
        &["tat", "zz-none", "--pick-stage", "kill"],
    )?
    .expect_success()?;

    sb.assert_host_untouched()?;
    check_timings(&t)
}

/// ctrl-d on an idle worktree: refused while dirty, removed once clean.
#[test]
#[ignore = "e2e: needs a built bn binary (cargo build --workspace)"]
fn tat_ctrl_d_refuses_a_dirty_worktree_and_removes_a_clean_one() -> anyhow::Result<()> {
    require_tmux!();
    let (sb, _bare, worktree) = picker_sandbox()?;
    let args = [
        "tat",
        "repo",
        "--pick-stage",
        "rmworktree",
        "--pick-arg",
        "worktree~feature-a",
    ];

    std::fs::write(worktree.join("scratch.txt"), "dirty\n")?;
    let dirty = sb.bn(args)?;
    assert_says(&dirty, "uncommitted change")?;
    assert!(worktree.is_dir(), "a dirty worktree must survive ctrl-d");

    std::fs::remove_file(worktree.join("scratch.txt"))?;
    let clean = sb.bn(args)?;
    assert_says(&clean, "✓ removed repo/feature-a")?;
    assert!(
        !worktree.exists(),
        "a clean worktree should be gone after ctrl-d"
    );
    assert!(
        !sb.bn(["tat", "repo", "--pick-stage", "sublist"])?
            .stdout
            .contains("worktree|feature-a|"),
        "the reload feed must not list the removed worktree"
    );

    sb.assert_host_untouched()
}
