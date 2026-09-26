//! Moving a slot to a new commit in a blobless partial clone, in steps that survive a reset link.
//!
//! In a clone made with `--filter=blob:none`, file contents come down on demand. A plain
//! `worktree add` or `reset --hard` onto a newer `main` asks for every missing blob at once — for
//! BigMono about 54,000 of them — as one lazy fetch that can't resume. On a link that drops long
//! transfers, that fetch never finishes; `main-sync`'s reset sat in exactly that for 15+ minutes.
//!
//! So the warm stages the move: point the slot at the commit without touching files (`reset
//! --soft`, or `worktree add --no-checkout` for a new slot), then `git backfill` the missing blobs
//! in batches, retrying — a batch that landed stays landed (a first try on BigMono kept 25,000 of
//! 54,478 blobs when the link reset) — and only then let the ordinary reset check the files out,
//! offline.

use std::path::Path;
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
            Err(e) => last = e,
        }
    }
    Err(format!("backfill gave up after {ATTEMPTS} tries: {last}"))
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
