//! git/clipboard/url side-effects + `bn diff`/`bn shot`/`bn autopush`.
use crate::*;

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

/// `bn diff` — git diff stat vs origin/main + the note's open todos/blockers + commits ahead.
/// A human convenience view, so it's text-only (no JSON envelope).
pub(crate) fn emit_diff(cfg: &Config, ctx: &Resolved) {
    println!("Changes ({}/{} vs main)", ctx.repo, ctx.branch);
    let stat = git_capture(&["diff", "--stat", "origin/main..HEAD"]);
    match stat.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => {
            for line in s.lines() {
                println!("  {line}");
            }
        }
        None => println!("  (no changes vs main)"),
    }

    if let Ok(note) = bn_core::store::read(&cfg.note_file(ctx)) {
        let open: Vec<_> = note
            .front
            .todos
            .iter()
            .filter(|t| t.is_open_todo())
            .collect();
        if !open.is_empty() {
            println!("\nOpen todos");
            for t in open {
                println!("  - [ ] {}", t.text);
            }
        }
        if !note.front.blockers.is_empty() {
            println!("\nBlockers");
            for b in &note.front.blockers {
                println!("  - {}", b.text);
            }
        }
    }

    let count = git_capture(&["rev-list", "--count", "origin/main..HEAD"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "0".to_string());
    println!("\n{count} commit(s) ahead of main");
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
/// delegate to the runner-backed [`crate::git::git_capture`]; the git reads it backs (`bn diff`'s
/// diff-stat / rev-list) stay driveable in core tests and `RealRunner` shells out exactly as
/// before. trim=false preserves the raw output these callers trim themselves.
pub(crate) fn git_capture(args: &[&str]) -> Option<String> {
    crate::git::git_capture(args, None, false)
}
