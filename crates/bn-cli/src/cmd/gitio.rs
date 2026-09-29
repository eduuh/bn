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

/// Copy rich HTML (with `plain` as the text alternative) so a paste into Teams, Outlook or a
/// browser editor keeps the hyperlinks. Returns false when no HTML-capable tool exists; the
/// caller then falls back to [`copy_to_clipboard`] with the plain text.
///
/// WSL goes first through Windows PowerShell, because the Windows clipboard is where Teams
/// reads from. Both payloads travel base64-encoded inside the command, so no quoting or console
/// codepage can mangle a non-ASCII name; the CF_HTML bytes go in as a stream so the byte offsets
/// in its header stay exact.
pub(crate) fn copy_html(html: &str, plain: &str) -> bool {
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms;\
         $h=[Convert]::FromBase64String('{}');\
         $t=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{}'));\
         $d=New-Object System.Windows.Forms.DataObject;\
         $d.SetData('HTML Format',(New-Object IO.MemoryStream(,$h)));\
         $d.SetData([System.Windows.Forms.DataFormats]::UnicodeText,$t);\
         [System.Windows.Forms.Clipboard]::SetDataObject($d,$true)",
        base64(cf_html(html).as_bytes()),
        base64(plain.as_bytes()),
    );
    for ps in [
        "powershell.exe",
        "/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe",
    ] {
        let ok = std::process::Command::new(ps)
            .args(["-NoProfile", "-NonInteractive", "-STA", "-Command", &script])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return true;
        }
    }
    let candidates: [(&str, &[&str]); 2] = [
        ("wl-copy", &["--type", "text/html"]),
        ("xclip", &["-selection", "clipboard", "-t", "text/html"]),
    ];
    for (cmd, args) in candidates {
        if let Ok(mut child) = std::process::Command::new(cmd)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            use std::io::Write;
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(html.as_bytes());
            }
            if child.wait().map(|s| s.success()).unwrap_or(false) {
                return true;
            }
        }
    }
    false
}

/// Wrap an HTML fragment in the Windows `CF_HTML` clipboard format: a header of byte offsets
/// (fixed ten-digit width, so the header's own length is known before the numbers are) followed
/// by a document whose fragment is marked with `StartFragment`/`EndFragment` comments.
pub(crate) fn cf_html(fragment: &str) -> String {
    const HEADER: &str = "Version:0.9\r\nStartHTML:{sh}\r\nEndHTML:{eh}\r\n\
                          StartFragment:{sf}\r\nEndFragment:{ef}\r\n";
    let pad = |n: usize| format!("{n:010}");
    let header_len = HEADER
        .replace("{sh}", &pad(0))
        .replace("{eh}", &pad(0))
        .replace("{sf}", &pad(0))
        .replace("{ef}", &pad(0))
        .len();
    let pre = "<html><body>\r\n<!--StartFragment-->";
    let post = "<!--EndFragment-->\r\n</body></html>";
    let start_fragment = header_len + pre.len();
    let end_fragment = start_fragment + fragment.len();
    let end_html = end_fragment + post.len();
    let header = HEADER
        .replace("{sh}", &pad(header_len))
        .replace("{eh}", &pad(end_html))
        .replace("{sf}", &pad(start_fragment))
        .replace("{ef}", &pad(end_fragment));
    format!("{header}{pre}{fragment}{post}")
}

/// Standard base64 (with padding). Only used to carry clipboard payloads into PowerShell, so a
/// dependency would be more weight than these dozen lines.
fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(A[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
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

#[cfg(test)]
mod clipboard_tests {
    use super::*;

    fn field(doc: &str, key: &str) -> usize {
        let line = doc.lines().find(|l| l.starts_with(key)).unwrap();
        line[key.len() + 1..].trim().parse().unwrap()
    }

    #[test]
    fn cf_html_offsets_slice_back_to_the_fragment() {
        let frag = r#"<a href="https://x.test/é">Ünïcode — name</a>"#;
        let doc = cf_html(frag);
        let bytes = doc.as_bytes();
        let (sf, ef) = (field(&doc, "StartFragment"), field(&doc, "EndFragment"));
        assert_eq!(&bytes[sf..ef], frag.as_bytes());
        assert_eq!(field(&doc, "EndHTML"), bytes.len());
        assert!(doc[field(&doc, "StartHTML")..].starts_with("<html>"));
    }

    #[test]
    fn base64_matches_the_rfc_vectors() {
        for (input, want) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), want);
        }
    }
}
