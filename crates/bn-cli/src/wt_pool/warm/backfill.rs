//! Moving a slot to a new commit in a blobless partial clone, in steps that survive a reset link.
//!
//! In a clone made with `--filter=blob:none`, file contents come down on demand. A plain
//! `worktree add` or `reset --hard` onto a newer `main` asks for every missing blob at once — for
//! a large monorepo about 54,000 of them — as one lazy fetch that can't resume. On a link that drops long
//! transfers, that fetch never finishes; `main-sync`'s reset sat in exactly that for 15+ minutes.
//!
//! So the warm stages the move: point the slot at the commit without touching files (`reset
//! --soft`, or `worktree add --no-checkout` for a new slot), then `git backfill` the missing blobs
//! in batches, retrying — a batch that landed stays landed (a first try on a large monorepo kept 25,000 of
//! 54,478 blobs when the link reset) — and only then let the ordinary reset check the files out,
//! offline.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

/// Tries for the backfill, counting the first. Each one resumes where the last stopped.
const ATTEMPTS: u32 = 6;
/// Wait before retry `n` (1-based) is `BACKOFF_SECS * n`. Zero in tests, which retry a remote
/// that is gone on purpose.
const BACKOFF_SECS: u64 = if cfg!(test) { 0 } else { 30 };
/// Blobs per request: big enough to be efficient, small enough that a reset loses little.
const MIN_BATCH: &str = "--min-batch-size=5000";

/// Whether `bare_s` is a partial clone whose missing objects are fetched from origin on demand.
pub(super) fn is_partial_clone(bare_s: &str) -> bool {
    crate::wt::git_out(&[
        "--git-dir",
        bare_s,
        "config",
        "--get",
        "remote.origin.promisor",
    ])
    .is_some_and(|v| v.trim() == "true")
}

/// Point `slot` at `sha` and fetch the blobs its tree needs, without checking anything out. A live
/// slot moves with `reset --soft`; anything else is created with `--no-checkout`. The caller's
/// ordinary reset then materialises the files from what is already local.
pub(super) fn stage_slot(bare_s: &str, slot: &Path, sha: &str) -> Result<(), String> {
    let slot_s = slot.to_string_lossy().to_string();
    let staged = if crate::wt_pool::is_live_slot(slot) {
        git(&["-C", &slot_s, "reset", "--quiet", "--soft", sha])
    } else {
        if let Some((_, n, pdir)) = crate::wt_pool::slot_parts(slot) {
            crate::wt_pool::health::clear_warm_stamp(&pdir, n);
        }
        git(&[
            "--git-dir",
            bare_s,
            "worktree",
            "add",
            "--quiet",
            "--no-checkout",
            "--detach",
            &slot_s,
            sha,
        ])
    };
    staged.map_err(|e| format!("could not point the slot at {sha}: {e}"))?;
    backfill(&slot_s)
}

/// Whether the checkout at `dir_s` belongs to a partial clone (its config is the bare repo's).
pub(crate) fn is_partial_checkout(dir_s: &str) -> bool {
    crate::wt::git_out(&["-C", dir_s, "config", "--get", "remote.origin.promisor"])
        .is_some_and(|v| v.trim() == "true")
}

/// Fetch every blob `target`'s tree needs into an existing checkout, so the caller's `reset
/// --hard` / `checkout` onto it runs offline. `backfill` only reads `HEAD`, so HEAD is pointed at
/// `target` with a soft reset first (files and index untouched) — and pointed back if the backfill
/// gives up. A checkout left with HEAD moved but the old files on disk would read as a pile of
/// uncommitted changes, and `main-sync` would snapshot and "discard" them on its next tick.
pub(crate) fn prefetch(dir_s: &str, target: &str) -> Result<(), String> {
    let before = crate::wt::git_out(&["-C", dir_s, "rev-parse", "HEAD"])
        .ok_or_else(|| format!("{dir_s} has no HEAD"))?;
    git(&["-C", dir_s, "reset", "--quiet", "--soft", target])
        .map_err(|e| format!("could not point HEAD at {target}: {e}"))?;
    let fetched = backfill(dir_s);
    if fetched.is_err() {
        let _ = git(&["-C", dir_s, "reset", "--quiet", "--soft", &before]);
    }
    fetched
}

fn backfill(slot_s: &str) -> Result<(), String> {
    let mut last = String::new();
    for attempt in 1..=ATTEMPTS {
        if attempt > 1 {
            eprintln!(
                "backfill attempt {}/{ATTEMPTS} failed ({last}) — resuming",
                attempt - 1
            );
            std::thread::sleep(Duration::from_secs(BACKOFF_SECS * u64::from(attempt - 1)));
        }
        match git(&["-C", slot_s, "backfill", MIN_BATCH]) {
            Ok(()) => return Ok(()),
            Err(e) if e.contains("not a git command") || e.contains("unknown subcommand") => {
                return backfill_missing_blobs(slot_s);
            }
            Err(e) => last = e,
        }
    }
    Err(format!("backfill gave up after {ATTEMPTS} tries: {last}"))
}

fn backfill_missing_blobs(slot_s: &str) -> Result<(), String> {
    let batch_size = MIN_BATCH
        .trim_start_matches("--min-batch-size=")
        .parse::<usize>()
        .unwrap_or(5000);
    let mut last = String::new();
    for attempt in 1..=ATTEMPTS {
        if attempt > 1 {
            eprintln!(
                "backfill attempt {}/{ATTEMPTS} failed ({last}) — resuming",
                attempt - 1
            );
            std::thread::sleep(Duration::from_secs(BACKOFF_SECS * u64::from(attempt - 1)));
        }
        let missing = missing_blobs(slot_s)?;
        if missing.is_empty() {
            return Ok(());
        }
        let mut fetch_failed = None;
        for chunk in missing.chunks(batch_size) {
            if let Err(e) = fetch_blob_batch(slot_s, chunk) {
                fetch_failed = Some(e);
                break;
            }
        }
        let remaining = missing_blobs(slot_s)?;
        if remaining.is_empty() {
            return Ok(());
        }
        last = fetch_failed.unwrap_or_else(|| format!("{} missing blobs remain", remaining.len()));
    }
    Err(format!("backfill gave up after {ATTEMPTS} tries: {last}"))
}

fn missing_blobs(slot_s: &str) -> Result<Vec<String>, String> {
    let out = crate::wt::git_out(&[
        "-C",
        slot_s,
        "rev-list",
        "--objects",
        "--missing=print",
        "HEAD",
    ])
    .ok_or_else(|| format!("could not inspect missing blobs in {slot_s}"))?;
    Ok(out
        .lines()
        .filter_map(|line| line.strip_prefix('?'))
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_string)
        .collect())
}

fn fetch_blob_batch(slot_s: &str, oids: &[String]) -> Result<(), String> {
    let mut cmd = crate::git::git_command(&["-C", slot_s, "cat-file", "--batch"]);
    bn_core::gitnet::stall_only(&mut cmd);
    run_blob_batch(&mut cmd, oids)
}

fn run_blob_batch(cmd: &mut std::process::Command, oids: &[String]) -> Result<(), String> {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn cat-file: {e}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "cat-file stdin missing".to_string())?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "cat-file stdout missing".to_string())?;
    let stdout_handle = std::thread::spawn(move || {
        let mut sink = std::io::sink();
        std::io::copy(&mut stdout, &mut sink).map(|_| ())
    });
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "cat-file stderr missing".to_string())?;
    let stderr_handle = std::thread::spawn(move || {
        let mut stderr = std::io::BufReader::new(stderr);
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut stderr, &mut buf).map(|_| buf)
    });
    let input = oids.iter().try_for_each(|oid| {
        std::io::Write::write_all(&mut stdin, oid.as_bytes())
            .and_then(|_| std::io::Write::write_all(&mut stdin, b"\n"))
    });
    drop(stdin);
    let status = child.wait();
    let stdout_result = stdout_handle.join();
    let stderr_result = stderr_handle.join();
    let status = status.map_err(|e| format!("wait cat-file: {e}"))?;
    stdout_result
        .map_err(|_| "cat-file stdout reader panicked".to_string())?
        .map_err(|e| format!("read cat-file stdout: {e}"))?;
    let stderr = stderr_result
        .map_err(|_| "cat-file stderr reader panicked".to_string())?
        .map_err(|e| format!("read cat-file stderr: {e}"))?;
    if !status.success() {
        return Err(format!(
            "cat-file exited with {status}: {}",
            String::from_utf8_lossy(&stderr).trim()
        ));
    }
    input.map_err(|e| format!("write cat-file input: {e}"))
}

/// One git call under the stall-only guard, its stderr kept for the error.
fn git(args: &[&str]) -> Result<(), String> {
    let mut cmd = crate::git::git_command(args);
    bn_core::gitnet::stall_only(&mut cmd);
    match cmd.output() {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            let tail: Vec<&str> = err
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();
            Err(tail[tail.len().saturating_sub(2)..].join(" / "))
        }
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_batch_drains_output_while_writing_input() {
        let mut cmd = std::process::Command::new("python3");
        cmd.args([
            "-c",
            r#"import signal, sys
signal.alarm(5)
sys.stdout.buffer.write(b"x" * 1048576)
sys.stdout.buffer.flush()
sys.stderr.buffer.write(b"x" * 1048576)
sys.stderr.buffer.flush()
assert len(sys.stdin.buffer.read()) == 205000
"#,
        ]);
        let oids = vec!["a".repeat(40); 5000];
        run_blob_batch(&mut cmd, &oids).expect("all pipes must make progress simultaneously");
    }

    #[test]
    fn blob_batch_preserves_child_failure() {
        let mut cmd = std::process::Command::new("python3");
        cmd.args([
            "-c",
            "import sys; sys.stderr.write('fetch failed'); sys.exit(7)",
        ]);
        let err = run_blob_batch(&mut cmd, &vec!["a".repeat(40); 5000]).unwrap_err();
        assert!(err.contains("exit status: 7"), "{err}");
        assert!(err.contains("fetch failed"), "{err}");
    }

    fn run(args: &[&str]) {
        assert!(crate::git::git_ok(args), "git {args:?}");
    }

    /// A slot in a blobless clone moves to a commit whose files it has never seen, and every file
    /// is local before anything is checked out.
    #[test]
    fn a_staged_slot_has_its_files_before_checkout() {
        let _env = bn_core::test_support::env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src").to_string_lossy().to_string();
        run(&["init", "-q", "-b", "main", &src]);
        run(&["-C", &src, "config", "uploadpack.allowFilter", "true"]);
        let commit = |name: &str| {
            std::fs::write(Path::new(&src).join(name), name).unwrap();
            run(&["-C", &src, "add", "-A"]);
            run(&[
                "-C",
                &src,
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                name,
            ]);
        };
        commit("a");
        let bare = tmp.path().join("bare.git").to_string_lossy().to_string();
        let url = format!("file://{src}");
        run(&["clone", "-q", "--bare", "--filter=blob:none", &url, &bare]);
        assert!(is_partial_clone(&bare));
        commit("b");
        run(&[
            "--git-dir",
            &bare,
            "fetch",
            "-q",
            "origin",
            "+refs/heads/main:refs/heads/main",
        ]);
        let sha = crate::wt::git_out(&["--git-dir", &bare, "rev-parse", "main"]).unwrap();

        let slot = tmp.path().join("_pool").join("2");
        stage_slot(&bare, &slot, &sha).expect("staged");

        let slot_s = slot.to_string_lossy().to_string();
        assert!(!slot.join("b").exists(), "nothing checked out yet");
        // Every blob of the tree is local: listing them with missing-object detection finds none.
        let missing = crate::wt::git_out(&[
            "-C",
            &slot_s,
            "rev-list",
            "--objects",
            "--missing=print",
            &sha,
        ])
        .unwrap_or_default();
        assert!(!missing.lines().any(|l| l.starts_with('?')), "{missing}");
        run(&["-C", &slot_s, "reset", "--quiet", "--hard", &sha]);
        assert_eq!(std::fs::read_to_string(slot.join("b")).unwrap(), "b");
    }

    /// A blobless bare clone of a one-commit repo, a checkout of that commit, and a newer commit
    /// fetched into the bare repo whose files the checkout has never seen.
    /// Returns `(tempdir, source, checkout, old_sha, new_sha)`.
    fn blobless_checkout() -> (tempfile::TempDir, String, String, String, String) {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src").to_string_lossy().to_string();
        run(&["init", "-q", "-b", "main", &src]);
        run(&["-C", &src, "config", "uploadpack.allowFilter", "true"]);
        let commit = |name: &str| {
            std::fs::write(Path::new(&src).join(name), name).unwrap();
            run(&["-C", &src, "add", "-A"]);
            run(&[
                "-C",
                &src,
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                name,
            ]);
            crate::wt::git_out(&["-C", &src, "rev-parse", "HEAD"]).unwrap()
        };
        let old = commit("a");
        let bare = tmp.path().join("bare.git").to_string_lossy().to_string();
        run(&[
            "clone",
            "-q",
            "--bare",
            "--filter=blob:none",
            &format!("file://{src}"),
            &bare,
        ]);
        let main = tmp.path().join("main").to_string_lossy().to_string();
        run(&[
            "--git-dir",
            &bare,
            "worktree",
            "add",
            "-q",
            "--detach",
            &main,
            &old,
        ]);
        let new = commit("b");
        run(&[
            "--git-dir",
            &bare,
            "fetch",
            "-q",
            "origin",
            "+refs/heads/main:refs/remotes/origin/main",
        ]);
        (tmp, src, main, old, new)
    }

    #[test]
    fn prefetch_leaves_head_on_target_with_every_blob_local() {
        let _env = bn_core::test_support::env_lock();
        let (_tmp, _src, main, _old, new) = blobless_checkout();
        assert!(is_partial_checkout(&main));

        prefetch(&main, &new).expect("prefetched");

        let missing = crate::wt::git_out(&[
            "-C",
            &main,
            "rev-list",
            "--objects",
            "--missing=print",
            &new,
        ])
        .unwrap_or_default();
        assert!(!missing.lines().any(|l| l.starts_with('?')), "{missing}");
        run(&["-C", &main, "reset", "--quiet", "--hard", &new]);
        assert!(Path::new(&main).join("b").exists());
    }

    #[test]
    fn fallback_fetches_missing_blobs_without_backfill_command() {
        let _env = bn_core::test_support::env_lock();
        let (_tmp, _src, main, _old, new) = blobless_checkout();
        run(&["-C", &main, "reset", "--quiet", "--soft", &new]);
        assert!(!missing_blobs(&main).unwrap().is_empty());

        backfill_missing_blobs(&main).expect("fallback prefetched");

        assert!(missing_blobs(&main).unwrap().is_empty());
        run(&["-C", &main, "reset", "--quiet", "--hard", &new]);
        assert_eq!(
            std::fs::read_to_string(Path::new(&main).join("b")).unwrap(),
            "b"
        );
    }

    /// When the files can't be fetched, the checkout must look exactly as it did: HEAD moved with
    /// the old files on disk would read as uncommitted changes to the next `main-sync` tick.
    #[test]
    fn a_failed_prefetch_puts_head_back() {
        let _env = bn_core::test_support::env_lock();
        let (_tmp, src, main, old, new) = blobless_checkout();
        std::fs::remove_dir_all(&src).unwrap();

        assert!(prefetch(&main, &new).is_err());

        assert_eq!(
            crate::wt::git_out(&["-C", &main, "rev-parse", "HEAD"]),
            Some(old)
        );
        assert_eq!(
            crate::wt::git_out(&["-C", &main, "status", "--porcelain"]).unwrap_or_default(),
            ""
        );
    }
}
