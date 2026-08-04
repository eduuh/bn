//! `bn kill-tmux` — reset tmux to a clean slate.
//!
//! Backs up the tmux-resurrect saved state to a tarball, wipes it (so tmux-continuum can't
//! auto-restore the old sessions on the next server start), then kills the tmux server. Run it
//! when you want tmux to come back up empty. Invoked from inside tmux, killing the server ends
//! the calling session too — which is the point.
//!
//! All tmux access routes through `crate::proc` (the sanctioned seam); the tarball is created with
//! a direct `tar` spawn, which the subprocess guard permits (only git/gh/tmux are seam-managed).

use crate::proc::{tmux, tmux_ok, tmux_run};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// `bn kill-tmux` — back up + wipe the resurrect state, then kill the tmux server. Returns the
/// process exit code (0 on success or a clean abort, non-zero only when the backup fails).
pub(crate) fn emit_kill_tmux(no_backup: bool, assume_yes: bool) -> i32 {
    let server_up = tmux_ok(&["info"]);
    let dir = resurrect_dir();
    let sessions = session_names();

    println!("bn kill-tmux — reset tmux to a clean slate:");
    match dir.as_deref() {
        Some(d) if has_contents(d) => println!(
            "  • resurrect state: {}{}",
            d.display(),
            if no_backup {
                "  (wipe, NO backup)"
            } else {
                "  (back up → wipe)"
            }
        ),
        Some(d) => println!("  • resurrect state: {} (already empty)", d.display()),
        None => println!("  • resurrect state: <none found>"),
    }
    if server_up {
        if sessions.is_empty() {
            println!("  • tmux server: running → kill-server");
        } else {
            println!(
                "  • tmux server: running ({} session(s): {}) → kill-server",
                sessions.len(),
                sessions.join(", ")
            );
        }
        if inside_tmux() {
            println!("  ⚠ you're inside tmux — this ends the current session.");
        }
    } else {
        println!("  • tmux server: not running");
    }

    if !confirm(assume_yes) {
        println!("Aborted.");
        return 0;
    }

    // 1. Back up the resurrect state before touching it — abort if the backup can't be written.
    if !no_backup {
        if let Some(d) = dir.as_deref() {
            if has_contents(d) {
                match backup_resurrect(d) {
                    Some(out) => println!("  ✓ backed up → {}", out.display()),
                    None => {
                        eprintln!("  ✗ backup failed — aborting before wipe.");
                        return 1;
                    }
                }
            }
        }
    }

    // 2. Neutralize save triggers so teardown can't regenerate the state we're about to wipe:
    // disable continuum auto-save and drop the resurrect save-on-close hooks.
    if server_up {
        let _ = tmux_run(&["set-option", "-g", "@continuum-save-interval", "0"]);
        let _ = tmux_run(&["set-hook", "-gu", "session-closed"]);
        let _ = tmux_run(&["set-hook", "-gu", "window-unlinked"]);
    }

    // 3. Wipe the saved state — nothing left for continuum to auto-restore on next start.
    if let Some(d) = dir.as_deref() {
        if d.is_dir() {
            match wipe_dir_contents(d) {
                Ok(n) if n > 0 => println!("  ✓ wiped {n} item(s) of resurrect state"),
                Ok(_) => {}
                Err(e) => eprintln!("  ✗ wipe error: {e}"),
            }
        }
    }

    // 4. Kill the server LAST: a call from inside tmux is terminated the moment this lands.
    if server_up {
        println!("  … killing tmux server");
        let _ = tmux_run(&["kill-server"]);
    }
    println!("  ✓ Done. tmux will start fresh next time.");
    0
}

/// Whether we're running inside a tmux client (so killing the server ends our own session).
fn inside_tmux() -> bool {
    std::env::var_os("TMUX").is_some_and(|v| !v.is_empty())
}

/// Names of the current tmux sessions (empty when no server is running).
fn session_names() -> Vec<String> {
    tmux(&["list-sessions", "-F", "#{session_name}"])
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Gate the destructive work behind an explicit yes: `--yes` skips it; otherwise require an
/// interactive `y`/`yes`. Non-interactive without `--yes` refuses (returns false).
fn confirm(assume_yes: bool) -> bool {
    if assume_yes {
        return true;
    }
    if !std::io::stdin().is_terminal() {
        eprintln!("Refusing to proceed non-interactively; pass --yes to confirm.");
        return false;
    }
    print!("Proceed? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut buf = String::new();
    if std::io::stdin().read_line(&mut buf).is_err() {
        return false;
    }
    matches!(buf.trim(), "y" | "Y" | "yes" | "Yes" | "YES")
}

/// True when `dir` exists and holds at least one entry.
fn has_contents(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut it| it.next().is_some())
        .unwrap_or(false)
}

/// Resolve the tmux-resurrect save directory: the explicit `@resurrect-dir` option when a server
/// is up, else the tmux-resurrect defaults (a pre-existing `~/.tmux/resurrect`, otherwise
/// `${XDG_DATA_HOME:-~/.local/share}/tmux/resurrect`).
fn resurrect_dir() -> Option<PathBuf> {
    if let Some(opt) = tmux(&["show-option", "-gqv", "@resurrect-dir"]) {
        let opt = opt.trim();
        if !opt.is_empty() {
            return Some(expand_tilde(opt));
        }
    }
    let home = dirs::home_dir()?;
    let xdg = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty());
    Some(resolve_resurrect_dir(&home, xdg.as_deref()))
}

/// Pure default-path resolution, mirroring tmux-resurrect: prefer a pre-existing
/// `~/.tmux/resurrect`, else `${XDG_DATA_HOME:-~/.local/share}/tmux/resurrect`.
fn resolve_resurrect_dir(home: &Path, xdg: Option<&Path>) -> PathBuf {
    let legacy = home.join(".tmux").join("resurrect");
    if legacy.is_dir() {
        return legacy;
    }
    let base = xdg
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local").join("share"));
    base.join("tmux").join("resurrect")
}

/// Expand a leading `~` / `$HOME` in a tmux option value to the real home directory.
fn expand_tilde(raw: &str) -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        if raw == "~" || raw == "$HOME" {
            return home;
        }
        if let Some(rest) = raw.strip_prefix("~/") {
            return home.join(rest);
        }
        if let Some(rest) = raw.strip_prefix("$HOME/") {
            return home.join(rest);
        }
    }
    PathBuf::from(raw)
}

/// Tar+gzip the whole resurrect dir into a timestamped sibling tarball, returning its path.
fn backup_resurrect(dir: &Path) -> Option<PathBuf> {
    let parent = dir.parent()?;
    let dir_name = dir.file_name()?;
    let stamp = bn_core::util::now_filestamp();
    let out = parent.join(format!("tmux-resurrect-backup-{stamp}.tar.gz"));
    // `tar` is not one of the seam-managed programs (git/gh/tmux), so a direct spawn is fine.
    let status = Command::new("tar")
        .arg("-czf")
        .arg(&out)
        .arg("-C")
        .arg(parent)
        .arg(dir_name)
        .status()
        .ok()?;
    status.success().then_some(out)
}

/// Remove everything inside `dir` (files, sub-dirs, and symlinks) but keep `dir` itself, so tmux
/// can write fresh state there later. Returns the count removed.
fn wipe_dir_contents(dir: &Path) -> std::io::Result<usize> {
    let entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    let mut removed = 0usize;
    for entry in entries {
        let path = entry.path();
        // file_type() does not follow symlinks, so a link (e.g. `last`) is unlinked, not its
        // target.
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
        removed += 1;
    }
    Ok(removed)
}


