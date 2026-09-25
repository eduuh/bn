//! `bn repos` — the repo fleet as one addressable thing (see [`crate::cli::ReposCmd`]).
//!
//! Two facts about the bare+worktree layout shape this command. First, the inventory already
//! exists: `<bare_dir>/<name>.git` is the only source that knows about a repo you cloned but have
//! taken no note on. Second, every managed clone keeps a worktree on its default branch at
//! `<worktree_dir>/<repo>/<default>`, so `foreach` has a real working copy to run in by default —
//! but the warm pool slots beside it (`_pool/N`) are checked out DETACHED, so finding that
//! worktree means matching `branch refs/heads/<default>` in the porcelain, not picking an entry
//! out of `worktree list`. A repo whose default worktree is missing falls back to the bare dir,
//! where plumbing (`fetch`, `log`, `for-each-ref`) still works.
use crate::*;
use std::path::{Path, PathBuf};
use std::process::Command;

const RED: &str = style::Tone::Danger.sgr();
const GREEN: &str = style::Tone::Ok.sgr();
const YELLOW: &str = style::Tone::Warn.sgr();
const NC: &str = style::RESET;

/// One repo's result from a `foreach` pass.
struct RunResult {
    repo: String,
    dir: PathBuf,
    /// `None` when the repo was skipped (no worktree on the requested branch).
    exit: Option<i32>,
    stdout: String,
    stderr: String,
}

impl RunResult {
    fn ok(&self) -> bool {
        self.exit == Some(0)
    }

    fn skipped(&self) -> bool {
        self.exit.is_none()
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "repo": self.repo,
            "dir": self.dir.to_string_lossy(),
            "exit": self.exit,
            "ok": self.ok(),
            "skipped": self.skipped(),
            "stdout": self.stdout,
            "stderr": self.stderr,
        })
    }
}

pub(crate) fn emit_repos(cfg: &Config, action: Option<ReposCmd>, json: bool) -> i32 {
    match action.unwrap_or(ReposCmd::List) {
        ReposCmd::List => list(cfg, json),
        ReposCmd::Foreach {
            worktree,
            bare,
            repo,
            jobs,
            argv,
        } => {
            let target = match (bare, worktree.as_deref()) {
                (true, _) => Target::Bare,
                (false, Some(b)) => Target::Branch(b),
                (false, None) => Target::Default,
            };
            foreach(cfg, &target, &repo, jobs, &argv, json)
        }
    }
}

/// Every cloned repo as `(name, bare dir)`, in name order.
fn fleet(cfg: &Config) -> Vec<(String, PathBuf)> {
    let (bare_dir, _) = crate::wt::layout(cfg);
    crate::wt::bare_repos(&bare_dir)
        .into_iter()
        .map(|bare| (crate::wt::repo_name(&bare), bare))
        .collect()
}

/// `git -C <dir> <args>` → trimmed stdout, or `None` when git failed.
fn git_in(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The worktrees git has registered for `bare`, as `(path, checked-out branch)`. Detached
/// checkouts (every warm pool slot) have no branch and are left out — `--worktree` selects by
/// branch name, so a slot that is on no branch can never be the target.
fn worktrees_of(bare: &Path) -> Vec<(PathBuf, String)> {
    crate::wt::worktree_rows(bare)
        .into_iter()
        .filter_map(|r| r.branch.map(|b| (r.path, b)))
        .collect()
}

fn list(cfg: &Config, json: bool) -> i32 {
    let repos = fleet(cfg);
    if repos.is_empty() {
        let (bare_dir, _) = crate::wt::layout(cfg);
        if json {
            json_ok(
                "repos",
                serde_json::json!({ "dir": bare_dir.to_string_lossy(), "repos": [] }),
            );
        } else {
            println!("No cloned repos under {}.", bare_dir.display());
        }
        return 0;
    }
    let rows: Vec<serde_json::Value> = repos
        .iter()
        .map(|(name, bare)| {
            let head = git_in(bare, &["symbolic-ref", "--short", "HEAD"]).unwrap_or_default();
            let branches = git_in(bare, &["for-each-ref", "--format=%(refname)", "refs/heads"])
                .map(|s| s.lines().count())
                .unwrap_or(0);
            let worktrees = worktrees_of(bare).len();
            let last = git_in(bare, &["log", "-1", "--format=%cr", &head]).unwrap_or_default();
            serde_json::json!({
                "repo": name,
                "git_dir": bare.to_string_lossy(),
                "default_branch": head,
                "branches": branches,
                "worktrees": worktrees,
                "last_commit": last,
            })
        })
        .collect();
    if json {
        let (bare_dir, _) = crate::wt::layout(cfg);
        json_ok(
            "repos",
            serde_json::json!({ "dir": bare_dir.to_string_lossy(), "repos": rows }),
        );
        return 0;
    }
    println!("{:<24} {:<14} {:>8} {:>4}  LAST COMMIT", "REPO", "DEFAULT", "BRANCHES", "WT");
    for row in &rows {
        println!(
            "{:<24} {:<14} {:>8} {:>4}  {}",
            row["repo"].as_str().unwrap_or(""),
            row["default_branch"].as_str().unwrap_or("-"),
            row["branches"].as_u64().unwrap_or(0),
            row["worktrees"].as_u64().unwrap_or(0),
            row["last_commit"].as_str().unwrap_or(""),
        );
    }
    0
}

/// Which directory a `foreach` pass runs in for one repo.
///
/// `Bare` is the git dir. `Branch` is the worktree checked out on that branch, and answers `None`
/// when the repo has none — the caller reports that as skipped, not failed. `Default` resolves the
/// repo's own default branch and falls back to the bare dir, so a repo that has lost its main
/// worktree still answers plumbing instead of dropping out of the fan-out.
enum Target<'a> {
    Bare,
    Default,
    Branch(&'a str),
}

fn target_dir(bare: &Path, target: &Target) -> Option<PathBuf> {
    let want = match target {
        Target::Bare => return Some(bare.to_path_buf()),
        Target::Branch(b) => (*b).to_string(),
        Target::Default => git_in(bare, &["symbolic-ref", "--short", "HEAD"])?,
    };
    let found = worktrees_of(bare)
        .into_iter()
        .find(|(path, b)| *b == want && path.is_dir())
        .map(|(path, _)| path);
    match target {
        Target::Default => found.or_else(|| Some(bare.to_path_buf())),
        _ => found,
    }
}

fn run_one(name: &str, bare: &Path, target: &Target, argv: &[String]) -> RunResult {
    let Some(dir) = target_dir(bare, target) else {
        return RunResult {
            repo: name.to_string(),
            dir: bare.to_path_buf(),
            exit: None,
            stdout: String::new(),
            stderr: String::new(),
        };
    };
    // `argv` is `required = true`, so clap has already rejected the empty case.
    let Some((program, rest)) = argv.split_first() else {
        return RunResult {
            repo: name.to_string(),
            dir,
            exit: Some(1),
            stdout: String::new(),
            stderr: "no command given".to_string(),
        };
    };
    let out = Command::new(program).args(rest).current_dir(&dir).output();
    match out {
        Ok(out) => RunResult {
            repo: name.to_string(),
            dir,
            // A signal-killed child reports no code; call that a failure rather than a success.
            exit: Some(out.status.code().unwrap_or(1)),
            stdout: String::from_utf8_lossy(&out.stdout).trim_end().to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).trim_end().to_string(),
        },
        Err(e) => RunResult {
            repo: name.to_string(),
            dir,
            exit: Some(127),
            stdout: String::new(),
            stderr: format!("{program}: {e}"),
        },
    }
}

/// Narrow the fleet to `wanted` (case-insensitive, `.git`-tolerant). An empty filter keeps all.
fn select(repos: Vec<(String, PathBuf)>, wanted: &[String]) -> Vec<(String, PathBuf)> {
    if wanted.is_empty() {
        return repos;
    }
    let wanted: Vec<String> = wanted
        .iter()
        .map(|w| w.trim_end_matches(".git").to_lowercase())
        .collect();
    repos
        .into_iter()
        .filter(|(name, _)| wanted.contains(&name.to_lowercase()))
        .collect()
}

fn foreach(
    cfg: &Config,
    target: &Target,
    only: &[String],
    jobs: usize,
    argv: &[String],
    json: bool,
) -> i32 {
    let repos = select(fleet(cfg), only);
    if repos.is_empty() {
        fail_json(
            json,
            "repos-foreach",
            "no_repos",
            "No repos matched. `bn repos list` shows the fleet.",
        );
    }
    // Hand each worker a contiguous slice of the fleet. `jobs` is clamped to at least one so
    // `--jobs 0` degrades to serial instead of dividing by zero.
    let jobs = jobs.max(1).min(repos.len());
    let chunk = repos.len().div_ceil(jobs);
    let mut results: Vec<RunResult> = std::thread::scope(|scope| {
        // Every worker is spawned before any is joined — collecting the handles lazily would
        // run the chunks one after another and make `--jobs` a no-op.
        let mut handles = Vec::with_capacity(jobs);
        for slice in repos.chunks(chunk) {
            handles.push(scope.spawn(move || {
                slice
                    .iter()
                    .map(|(name, bare)| run_one(name, bare, target, argv))
                    .collect::<Vec<_>>()
            }));
        }
        let mut out = Vec::with_capacity(repos.len());
        for handle in handles {
            if let Ok(chunk) = handle.join() {
                out.extend(chunk);
            }
        }
        out
    });
    results.sort_by(|a, b| a.repo.cmp(&b.repo));

    let failed = results.iter().filter(|r| !r.ok() && !r.skipped()).count();
    if json {
        let rows: Vec<serde_json::Value> = results.iter().map(RunResult::to_json).collect();
        json_ok(
            "repos-foreach",
            serde_json::json!({
                "repos": rows,
                "failed": failed,
                "skipped": results.iter().filter(|r| r.skipped()).count(),
            }),
        );
    } else {
        for r in &results {
            render_one(r, target);
        }
        println!(
            "\n{}/{} ok, {failed} failed",
            results.len() - failed - results.iter().filter(|r| r.skipped()).count(),
            results.len()
        );
    }
    i32::from(failed > 0)
}

/// One repo's human-readable block: a header line, then whatever the command said.
fn render_one(r: &RunResult, target: &Target) {
    if r.skipped() {
        let want = match target {
            Target::Branch(b) => *b,
            _ => "the requested branch",
        };
        cprintln!("{YELLOW}{} — skipped (no worktree on {want}){NC}", r.repo);
        return;
    }
    let exit = r.exit.unwrap_or(1);
    if r.ok() {
        cprintln!("{GREEN}{}{NC}", r.repo);
    } else {
        cprintln!("{RED}{} — exit {exit}{NC}", r.repo);
    }
    for line in r.stdout.lines().chain(r.stderr.lines()) {
        println!("  {line}");
    }
}

#[cfg(test)]
mod tests {
    use super::select;
    use std::path::PathBuf;

    fn fleet() -> Vec<(String, PathBuf)> {
        ["atlas", "bn", "wira360"]
            .iter()
            .map(|n| ((*n).to_string(), PathBuf::from(format!("/p/bare/{n}.git"))))
            .collect()
    }

    #[test]
    fn an_empty_filter_keeps_the_whole_fleet() {
        assert_eq!(select(fleet(), &[]).len(), 3);
    }

    #[test]
    fn names_are_matched_case_insensitively_and_tolerate_a_git_suffix() {
        let picked = select(fleet(), &["BN".to_string(), "wira360.git".to_string()]);
        let names: Vec<&str> = picked.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["bn", "wira360"]);
    }

    #[test]
    fn a_name_that_matches_nothing_selects_nothing() {
        assert!(select(fleet(), &["nope".to_string()]).is_empty());
    }
}
