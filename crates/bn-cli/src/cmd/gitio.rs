//! git/clipboard/url side-effects + `bn diff`/`bn shot`/`bn autopush`.
use crate::*;
use std::path::{Path, PathBuf};

/// Copy text to the clipboard, trying each platform tool in the oracle's order. Returns false
/// if none is available (caller then prints the value as a fallback).
pub(crate) fn copy_to_clipboard(text: &str) -> bool {
    let candidates: [(&str, &[&str]); 4] = [
        ("pbcopy", &[]),
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("/mnt/c/Windows/System32/clip.exe", &[]),
    ];
    for (cmd, args) in candidates {
        if let Ok(mut child) = std::process::Command::new(cmd)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            use std::io::Write;
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(text.as_bytes());
            }
            if child.wait().map(|s| s.success()).unwrap_or(false) {
                return true;
            }
        }
    }
    false
}

/// Open a URL in the default browser, trying each platform opener in the oracle's order.
pub(crate) fn open_url(url: &str) -> bool {
    for (cmd, background) in [
        ("open", false),
        ("wslview", false),
        ("xdg-open", true),
        ("/mnt/c/Windows/explorer.exe", false),
    ] {
        let mut c = std::process::Command::new(cmd);
        c.arg(url);
        if background {
            c.stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            if c.spawn().is_ok() {
                return true;
            }
        } else if c.status().map(|s| s.success()).unwrap_or(false) {
            return true;
        }
    }
    false
}

/// `bn shot [--gif] [--fresh <secs>]` — move the freshest capture(s) into the branch note's
/// folder and embed the markdown links. "Nothing fresh" exits 2 (a normal "capture first"
/// outcome), other failures exit 1.
pub(crate) fn emit_shot(cfg: &Config, ctx: &Resolved, gif: bool, fresh: u64, as_json: bool) {
    let kind = if gif {
        capture::Capture::Gif
    } else {
        capture::Capture::Screenshot
    };
    match capture::grab(cfg, ctx, kind, fresh, &today()) {
        Ok(o) => {
            if as_json {
                let data = serde_json::json!({
                    "kind": o.kind,
                    "moved": o.moved,
                    "heading": o.heading,
                });
                println!("{}", Envelope::success("shot", data).to_json());
            } else {
                println!("Grabbed {} {}(s):", o.moved.len(), o.kind);
                for rel in &o.moved {
                    println!("  {rel}");
                }
                println!("Embedded under {} in note.md", o.heading);
            }
        }
        Err(e) => {
            if as_json {
                let code = match e {
                    capture::GrabError::NothingFresh(..) => "nothing_fresh",
                    capture::GrabError::Unconfigured(_) => "unconfigured",
                    capture::GrabError::SourceMissing(_) => "source_missing",
                    capture::GrabError::Io(_) => "io",
                };
                println!("{}", Envelope::failure("shot", code, e.message()).to_json());
            } else {
                eprintln!("{}", e.message());
            }
            std::process::exit(e.exit_code());
        }
    }
}

/// `bn autopush [msg]` — stage tracked changes, commit, and push if there's an upstream.
/// Ports cmd_autopush. Returns the process exit code.
pub(crate) fn emit_autopush(message: &str) -> i32 {
    let msg = if message.trim().is_empty() {
        format!("wip: auto-commit {}", bn_core::util::now_stamp())
    } else {
        message.to_string()
    };
    if !git_status(&["add", "-u"]) {
        eprintln!("git add failed");
        return 1;
    }
    // Nothing staged → clean exit, no empty commit (diff --cached --quiet exits 0 when clean).
    if git_status(&["diff", "--cached", "--quiet"]) {
        println!("No tracked changes to commit.");
        return 0;
    }
    if !git_status(&["commit", "-m", &msg]) {
        eprintln!("Commit failed");
        return 1;
    }
    match git_capture(&[
        "rev-parse",
        "--abbrev-ref",
        "--symbolic-full-name",
        "@{upstream}",
    ]) {
        Some(u) if !u.trim().is_empty() => {
            if !git_status(&["push"]) {
                eprintln!("Push failed");
                return 1;
            }
        }
        _ => println!("No upstream — committed locally, skipping push."),
    }
    0
}

/// Run `git <args>` in the cwd, inheriting stdio; true if it exited 0. Delegates to
/// [`wt::run_git`] (byte-identical body) rather than the capturing runner seam: `bn save`'s
/// `commit`/`push` must show their live output on the terminal, which the capture-only runner
/// can't reproduce.
pub(crate) fn git_status(args: &[&str]) -> bool {
    wt::run_git(args)
}

/// Where `bn diff` should look. `dir` is the worktree to run git in; `ctx` keys the note whose
/// todos/blockers are shown beside the stat. They travel together because a `--path` into another
/// worktree must move *both* — showing this branch's todos next to that branch's diff is the
/// picker-lies failure this whole feature exists to avoid.
pub(crate) struct DiffTarget {
    pub(crate) ctx: Resolved,
    pub(crate) dir: PathBuf,
}

/// Resolve `bn diff`'s target from its flags: an explicit `--path`, an explicit `--repo/--branch`
/// (looked up through the rollup so a pooled slot resolves to its real checkout rather than a
/// reconstructed `<worktree_dir>/<repo>/<branch>` that would not exist), else the cwd.
pub(crate) fn diff_target(
    cfg: &Config,
    path: Option<&Path>,
    repo: Option<&str>,
    branch: Option<&str>,
) -> Result<DiffTarget, String> {
    if let (Some(repo), Some(branch)) = (repo, branch) {
        let entry = bn_core::rollups::worktrees(cfg)
            .into_iter()
            .find(|w| w.repo == repo && w.branch == branch)
            .ok_or_else(|| format!("no note for {repo}/{branch}"))?;
        let dir = entry
            .worktree
            .ok_or_else(|| format!("{repo}/{branch} has no worktree on this machine"))?;
        return Ok(DiffTarget {
            ctx: Resolved {
                repo: repo.to_string(),
                branch: branch.to_string(),
            },
            dir: PathBuf::from(dir),
        });
    }

    let dir = match path {
        Some(p) => p.canonicalize().unwrap_or_else(|_| p.to_path_buf()),
        None => std::env::current_dir().map_err(|e| e.to_string())?,
    };
    let ctx = bn_core::resolver::resolve(cfg, &dir).map_err(|_| format!("{} is not in a git repo", dir.display()))?;
    Ok(DiffTarget { ctx, dir })
}

/// `bn diff` — diff stat vs the branch's base + the note's open todos/blockers + commits ahead.
///
/// The base comes from [`bn_core::gitbase::base_ref`] rather than a hardcoded `origin/main`, and
/// `three_dot` selects `<base>...HEAD` — changes on this branch since it diverged. Two-dot (the
/// historical default, kept until the flip is made deliberately) reports the base's own newer
/// commits as "your changes" on any worktree that has fallen behind.
pub(crate) fn emit_diff(
    cfg: &Config,
    target: &DiffTarget,
    base: Option<&str>,
    three_dot: bool,
    as_json: bool,
) {
    let dir = target.dir.as_path();
    let ctx = &target.ctx;
    let base = base
        .map(str::to_string)
        .or_else(|| {
            bn_core::gitbase::base_ref(
                &bn_core::runner::RealRunner,
                bn_core::gitbase::BaseScope::Worktree(dir),
            )
        })
        .unwrap_or_else(|| "origin/main".to_string());
    let range = if three_dot {
        bn_core::gitbase::range_to_head(&base)
    } else {
        format!("{base}..HEAD")
    };

    let stat = git_capture_in(&["diff", "--stat", &range], dir);
    let stat_lines: Vec<String> = stat
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        // git's `--stat` indents every row by one space; trimming the block would de-indent only
        // the first, so normalize per line and let the printer own the indent.
        .map(|s| s.lines().map(|l| l.trim().to_string()).collect())
        .unwrap_or_default();

    let count: u64 = git_capture_in(&["rev-list", "--count", &range], dir)
        .map(|s| s.trim().to_string())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let note = bn_core::store::read(&cfg.note_file(ctx)).ok();
    let todos: Vec<String> = note
        .as_ref()
        .map(|n| {
            n.front
                .todos
                .iter()
                .filter(|t| t.is_open_todo())
                .map(|t| t.text.clone())
                .collect()
        })
        .unwrap_or_default();
    let blockers: Vec<String> = note
        .as_ref()
        .map(|n| n.front.blockers.iter().map(|b| b.text.clone()).collect())
        .unwrap_or_default();

    if as_json {
        let env = bn_core::render::Envelope::success(
            "diff",
            serde_json::json!({
                "repo": ctx.repo,
                "branch": ctx.branch,
                "worktree": dir.display().to_string(),
                "base": base,
                "range": range,
                "stat": stat_lines,
                "commits_ahead": count,
                "open_todos": todos,
                "blockers": blockers,
            }),
        );
        println!("{}", env.to_json());
        return;
    }

    println!("Changes ({}/{} vs {base})", ctx.repo, ctx.branch);
    if stat_lines.is_empty() {
        println!("  (no changes vs {base})");
    } else {
        for line in &stat_lines {
            println!("  {line}");
        }
    }

    if !todos.is_empty() {
        println!("\nOpen todos");
        for t in &todos {
            println!("  - [ ] {t}");
        }
    }
    if !blockers.is_empty() {
        println!("\nBlockers");
        for b in &blockers {
            println!("  - {b}");
        }
    }

    println!("\n{count} commit(s) ahead of {base}");
}

/// Restore the default SIGPIPE disposition on Unix. Rust ignores SIGPIPE by default, so a
/// closed downstream pipe (`bn todo | head`, `bn diff | grep -q`) turns a write into an
/// `EPIPE` error that the print macros then panic on. Resetting to `SIG_DFL` makes bn die
/// quietly like a normal Unix filter instead — matching the bash bn, which never panicked.
#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "restoring SIGPIPE's default disposition requires the libc signal API"
)]
pub(crate) fn reset_sigpipe() {
    // SAFETY: setting a signal handler to the default disposition is async-signal-safe and
    // has no memory-safety implications.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

#[cfg(not(unix))]
pub(crate) fn reset_sigpipe() {}

/// Run `git <args>` in the cwd, returning stdout (raw, untrimmed) on success, else None. Thin
/// delegate to the runner-backed [`crate::git::git_capture`], so the reads it backs stay driveable
/// in core tests while `RealRunner` shells out exactly as before. trim=false preserves the raw
/// output these callers trim themselves.
pub(crate) fn git_capture(args: &[&str]) -> Option<String> {
    crate::git::git_capture(args, None, false)
}

/// [`git_capture`] scoped to `dir` (`git -C <dir>`) — what lets `bn diff` describe a worktree
/// other than the one bn is running in, which is the whole point of `--path`/`--repo`.
pub(crate) fn git_capture_in(args: &[&str], dir: &std::path::Path) -> Option<String> {
    crate::git::git_capture(args, Some(dir), false)
}
