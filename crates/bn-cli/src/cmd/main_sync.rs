//! `main-sync` — keep every repo's `main` checkout present and current, every 15 minutes.
//!
//! The main worktree (`<root>/worktree/<repo>/main`) is load-bearing and unvisited: stow reads it,
//! the `pre-commit` hook keys on its shape, and new work is cut from the branch it holds. Nobody
//! is supposed to open it — [`crate::main_slot`] is where main-branch work happens — so its only
//! job is to *exist*, clean and current.
//!
//! Left alone it does neither. A checkout drifts behind the moment `origin/main` moves, and a
//! stray edit made before this split existed pins it there for good: `merge --ff-only` refuses a
//! dirty tree, so main rots silently and every branch cut from it starts stale. That is not a
//! hypothetical — the checkout this job was written on sat six commits behind with five staged
//! files, all of them already merged upstream.
//!
//! So this job treats local change in the main worktree as debris, and says so:
//!
//! 1. fetch the bare clone;
//! 2. create the main worktree if the repo hasn't got one — "always have a main checkout" is the
//!    whole point, and a repo cloned by hand never gets one otherwise;
//! 3. `reset --hard` it onto the remote tip, naming every tracked file it discards;
//! 4. advance the repo's main slot (`_pool/1`) to the same commit, unless someone is in it.
//!
//! The reset is recoverable. Anything it is about to discard is committed first to
//! `refs/bn/main-rescue` via `git stash create` — which writes the commit without touching the
//! working tree, the index or the shared stash stack — and the log line says how to get it back.
//! A job that runs unattended every quarter hour must not be one bad assumption away from losing
//! work nobody knew was there.
//!
//! Two deliberate limits on the destruction. **Untracked files are left alone** — they never block
//! a fast-forward or a `worktree add`, so removing them would buy nothing and could take a scratch
//! file with it, and they are not counted as dirt either. And a main worktree sitting on some
//! *other* branch is skipped entirely: that is a human decision the job has no business overruling.

use crate::wt;
use bn_core::resolver::{Config, Resolved};
use std::path::Path;

use super::jobs::native_job_line;

/// What one repo's main worktree needs. Pure so the "when do we discard?" rule is testable without
/// a git repo behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MainSync {
    /// No checkout yet — cut one at the default branch.
    Create,
    /// Clean and on the default branch: reset it onto the remote tip.
    Advance,
    /// On the default branch with `n` tracked changes — discard them (loudly) and advance.
    Discard(usize),
    /// Sitting on some other branch. Somebody moved it deliberately; leave it, and say so.
    Skip(String),
}

/// The plan for a main worktree that is `exists`, checked out on `head` (`None` when detached or
/// unreadable), with `dirty` tracked changes. `default` is the repo's default branch.
pub(crate) fn plan(exists: bool, head: Option<&str>, dirty: usize, default: &str) -> MainSync {
    if !exists {
        return MainSync::Create;
    }
    match head {
        Some(branch) if branch == default => {
            if dirty > 0 {
                MainSync::Discard(dirty)
            } else {
                MainSync::Advance
            }
        }
        Some(branch) => MainSync::Skip(branch.to_string()),
        // Detached: not the branch the rest of bn expects to find here, and not ours to reattach.
        None => MainSync::Skip("a detached HEAD".to_string()),
    }
}

/// `main-sync` — the 15-minute sweep. Every bare repo under the configured root, in name order;
/// one repo's failure never stops the next. Always exits 0 unless a git command that should have
/// worked didn't: a fetch that can't reach the network is a fact about the network, not a broken
/// cron, and paging on it every quarter hour would train the operator to ignore the job.
pub(crate) fn job_main_sync(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let (bare_dir, worktree_dir) = wt::layout(cfg);
    let mut rc = 0;
    for bare in wt::bare_repos(&bare_dir) {
        let repo = wt::repo_name(&bare);
        if !sync_repo(&bare, &worktree_dir, &repo) {
            rc = 1;
        }
    }
    rc
}

/// Bring one repo's main checkout and main slot up to date. `false` when something that should
/// have worked failed (the caller turns that into a non-zero exit).
///
/// `pub(crate)` because `bn reset` needs exactly this before it tears the other worktrees down,
/// and used to hand-roll a weaker version: a fetch plus `merge --ff-only origin/main`, which
/// hardcoded the branch name and gave up silently on the dirty checkouts this exists to fix.
pub(crate) fn sync_repo(bare: &Path, worktree_dir: &Path, repo: &str) -> bool {
    let bare_s = bare.to_string_lossy().to_string();
    if let Err(why) = crate::origin_fetch::refresh_origin(&bare_s) {
        // Offline, an auth failure, or a repo with no reachable remote. Everything below still
        // works against whatever was last fetched, so carry on rather than skipping the repo — but
        // say what git said. "offline?" hid a GitHub auth failure and BigMono's shallow-clone trap.
        native_job_line(&format!("main-sync: {repo}: fetch failed: {why}"), false);
    }
    let Some(default) = default_branch(&bare_s) else {
        native_job_line(
            &format!("main-sync: {repo}: no default branch — skipped"),
            true,
        );
        return false;
    };
    let target = remote_tip(&bare_s, &default);
    let main = crate::main_slot::main_worktree(worktree_dir, repo);
    let main_s = main.to_string_lossy().to_string();
    let live_main = crate::wt_pool::is_live_slot(&main);
    let head = if live_main {
        head_branch(&main_s)
    } else {
        None
    };
    let tracked_dirty = if live_main { dirty(&main_s) } else { 0 };

    let ok = match plan(live_main, head.as_deref(), tracked_dirty, &default) {
        MainSync::Create => {
            create_main(&bare_s, worktree_dir, &main, repo, &default)
                && advance(&main_s, &target, repo, "main worktree")
        }
        MainSync::Skip(head) => {
            native_job_line(
                &format!("main-sync: {repo}: main worktree is on {head}, not {default} — left alone"),
                false,
            );
            true
        }
        MainSync::Discard(n) => {
            for path in tracked_changes(&main_s).iter().take(DISCARD_REPORT_LIMIT) {
                native_job_line(&format!("main-sync: {repo}: discarding {path}"), false);
            }
            if n > DISCARD_REPORT_LIMIT {
                let rest = n - DISCARD_REPORT_LIMIT;
                native_job_line(&format!("main-sync: {repo}: …and {rest} more"), false);
            }
            let Some(rescue) = snapshot(&main_s) else {
                // Refusing is the whole point. The alternative — reset anyway — is what silently
                // destroyed uncommitted work in every repo on this machine for two days, because
                // the snapshot had been failing since #1262 and nothing said so. A main worktree
                // left un-advanced is visible and recoverable; work reset without a snapshot is
                // neither. Loud (stderr → `bn cron failures`) and retried next tick.
                native_job_line(
                    &format!(
                        "main-sync: {repo}: REFUSING to discard {n} change(s) — could not snapshot them to {RESCUE_REF}. \
Commit or clean {main_s} yourself; nothing was discarded."
                    ),
                    true,
                );
                return false;
            };
            native_job_line(
                &format!("main-sync: {repo}: recover them with `git -C {main_s} stash apply {rescue}` ({RESCUE_REF})"),
                false,
            );
            advance(&main_s, &target, repo, "main worktree")
        }
        MainSync::Advance => advance(&main_s, &target, repo, "main worktree"),
    };
    sync_main_slot(bare, worktree_dir, repo, &target) && ok
}

/// How many discarded paths to name before summarising the rest. Enough to recognise what was
/// thrown away; short enough that a badly-drifted checkout can't bury the rest of the sweep.
const DISCARD_REPORT_LIMIT: usize = 10;

/// Where the pre-reset snapshot is kept. Outside `refs/heads` and `refs/remotes`, so it shows up
/// in neither the branch list nor a push, and outside the stash stack, which bn shares across
/// worktrees and warns against touching.
const RESCUE_REF: &str = "refs/bn/main-rescue";

/// Commit the checkout's uncommitted state to [`RESCUE_REF`] and return the sha, before anything
/// discards it.
///
/// `git stash create` builds the commit **without** touching the working tree, the index, or the
/// stash stack — which makes it exactly the right tool here and nothing like `git stash`. The
/// reset that follows is still a reset; this only means the operator who finds out an hour later
/// has somewhere to go. `None` when there was nothing to snapshot or git declined, in which case
/// the reset proceeds anyway: the snapshot is a courtesy, not a precondition.
fn snapshot(dir_s: &str) -> Option<String> {
    let sha = crate::git::git_capture(
        &["-C", dir_s, "stash", "create", "bn main-sync rescue"],
        None,
        true,
    )
    .filter(|s| !s.is_empty())?;
    if write_rescue_ref(dir_s, &sha) {
        return Some(sha);
    }
    // The one failure this has actually hit: a legacy dated rescue occupying the name as a
    // DIRECTORY. Clear it (preserving the commits) and try once more.
    free_legacy_rescue_refs(dir_s);
    write_rescue_ref(dir_s, &sha).then_some(sha)
}

fn write_rescue_ref(dir_s: &str, sha: &str) -> bool {
    crate::git::git_ok(&[
        "-C",
        dir_s,
        "update-ref",
        RESCUE_REF,
        sha,
        "-m",
        "main-sync: uncommitted work in the main worktree, before reset",
    ])
}

/// Move legacy `refs/bn/main-rescue/<date>` refs aside to flat `refs/bn/main-rescue-<date>`.
///
/// git's ref store is a filesystem: `refs/bn/main-rescue` cannot be a file while
/// `refs/bn/main-rescue/2026-09-04` makes it a directory. Rescues written before #1262 used the
/// dated form, so the moment #1262 switched to the flat name every subsequent rescue failed with
/// `cannot lock ref … exists; cannot create`, in every repo that had ever been rescued — and
/// [`snapshot`] swallowed it, so `main-sync` went on discarding uncommitted work with no snapshot
/// and no warning for anyone to notice.
///
/// Renamed rather than deleted: those commits are somebody's rescued work, and this runs
/// unattended. A flat sibling keeps them reachable (`git stash apply refs/bn/main-rescue-<date>`)
/// and frees the name for good, so this is a one-time self-heal rather than a permanent tax.
fn free_legacy_rescue_refs(dir_s: &str) {
    let pattern = format!("{RESCUE_REF}/**");
    let Some(listed) = crate::git::git_capture(
        &["-C", dir_s, "for-each-ref", &pattern, "--format=%(refname)"],
        None,
        true,
    ) else {
        return;
    };
    for old in listed.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Some(leaf) = old.rsplit('/').next() else {
            continue;
        };
        let new = format!("{RESCUE_REF}-{leaf}");
        let Some(sha) = crate::git::git_capture(
            &["-C", dir_s, "rev-parse", &format!("{old}^{{}}")],
            None,
            true,
        ) else {
            continue;
        };
        if crate::git::git_ok(&["-C", dir_s, "update-ref", &new, &sha])
            && crate::git::git_ok(&["-C", dir_s, "update-ref", "-d", old])
        {
            native_job_line(
                &format!("main-sync: moved legacy rescue {old} → {new}"),
                false,
            );
        }
    }
}

/// Cut the repo's missing main worktree at `default`.
fn create_main(bare_s: &str, worktree_dir: &Path, main: &Path, repo: &str, default: &str) -> bool {
    if let Some(parent) = main.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if main.exists() && !crate::wt_pool::is_live_slot(main) {
        crate::wt_pool::force_remove_worktree(bare_s, main);
    }
    if move_legacy_main_worktree(bare_s, worktree_dir, main, repo, default) {
        return true;
    }
    let main_s = main.to_string_lossy().to_string();
    if wt::create_worktree(bare_s, default, &main_s, false) {
        native_job_line(&format!("main-sync: {repo}: created main worktree"), false);
        true
    } else {
        native_job_line(
            &format!("main-sync: {repo}: could not create main worktree"),
            true,
        );
        false
    }
}

/// Move a clean legacy `<repo>/<default>` main worktree into the canonical `<repo>/main` path.
///
/// The directory name is always `main`, whatever branch it holds; older repos can still have a
/// default-branch-named worktree (`…/master`). Creating `…/main` beside it fails when that older
/// checkout still has the default branch checked out, because git will not check one branch out
/// twice. A clean legacy checkout is the same main worktree under the old name, so move it
/// in-place instead of trying to create a second copy.
fn move_legacy_main_worktree(
    bare_s: &str,
    worktree_dir: &Path,
    main: &Path,
    repo: &str,
    default: &str,
) -> bool {
    if default.eq_ignore_ascii_case("main") {
        return false;
    }
    let legacy = worktree_dir.join(repo).join(default);
    if legacy == main || !crate::wt_pool::is_live_slot(&legacy) {
        return false;
    }
    let legacy_s = legacy.to_string_lossy().to_string();
    if head_branch(&legacy_s).as_deref() != Some(default) || dirty(&legacy_s) > 0 {
        return false;
    }
    let main_s = main.to_string_lossy().to_string();
    if crate::git::git_ok(&["--git-dir", bare_s, "worktree", "move", &legacy_s, &main_s]) {
        native_job_line(
            &format!("main-sync: {repo}: moved legacy {default} worktree to main"),
            false,
        );
        true
    } else {
        false
    }
}

/// Move a checkout onto `target`, discarding whatever tracked state it held. Silent on a no-op:
/// this runs every fifteen minutes across every repo, and "already current" is not news.
fn advance(dir_s: &str, target: &str, repo: &str, what: &str) -> bool {
    let before = head_sha(dir_s);
    if !prefetch_blobs(dir_s, target, repo, what) {
        return false;
    }
    if !crate::git::git_ok(&["-C", dir_s, "reset", "--hard", "--quiet", target]) {
        native_job_line(
            &format!("main-sync: {repo}: could not reset {what} to {target}"),
            true,
        );
        return false;
    }
    let after = head_sha(dir_s);
    if before != after {
        native_job_line(&format!("main-sync: {repo}: {what} → {target}"), false);
    }
    true
}

/// In a blobless partial clone, fetch the files `target` needs before the reset or checkout that
/// moves onto it. Left to the reset, they come down as one lazy fetch that can't resume — on BigMono,
/// 54k blobs over a link that drops long transfers, and the tick sat in it for 15+ minutes. The
/// backfill goes in resumable batches, and on failure the checkout stays where it was, untouched,
/// for the next tick to retry. A no-op when the checkout is already there or the clone is whole.
fn prefetch_blobs(dir_s: &str, target: &str, repo: &str, what: &str) -> bool {
    use crate::wt_pool::warm::backfill;
    let at_target = crate::wt::git_out(&["-C", dir_s, "rev-parse", target]);
    if at_target.is_none() || at_target == head_sha(dir_s) || !backfill::is_partial_checkout(dir_s) {
        return true;
    }
    match backfill::prefetch(dir_s, target) {
        Ok(()) => true,
        Err(why) => {
            native_job_line(
                &format!("main-sync: {repo}: could not fetch the files for {what} → {target}, left where it was: {why}"),
                true,
            );
            false
        }
    }
}

/// Create the repo's main slot if it hasn't got one, then advance it to the same commit.
///
/// The slot is a *detached* checkout — git will not check `main` out twice, and the main worktree
/// holds the branch — so it is moved with a detaching reset rather than a merge. Skipped whenever
/// somebody could lose something by it: uncommitted work in the slot, or a live agent (`bn
/// control` lives here) working in it.
fn sync_main_slot(bare: &Path, worktree_dir: &Path, repo: &str, target: &str) -> bool {
    let Some(slot) = crate::main_slot::ensure(bare, worktree_dir, repo) else {
        // No bare clone, opted out of pooling, or the checkout could not be created — all of them
        // already reported by whoever cares. Not a failure of this job.
        return true;
    };
    let slot_s = slot.to_string_lossy().to_string();
    if dirty(&slot_s) > 0 {
        native_job_line(
            &format!("main-sync: {repo}: main slot has uncommitted work — left alone"),
            false,
        );
        return true;
    }
    if !crate::agent_lease::live_agents_in(&slot).is_empty() {
        return true;
    }
    let before = head_sha(&slot_s);
    if !prefetch_blobs(&slot_s, target, repo, "main slot") {
        return false;
    }
    if !crate::git::git_ok(&["-C", &slot_s, "checkout", "--quiet", "--detach", target]) {
        native_job_line(
            &format!("main-sync: {repo}: could not advance the main slot to {target}"),
            true,
        );
        return false;
    }
    if before != head_sha(&slot_s) {
        native_job_line(&format!("main-sync: {repo}: main slot → {target}"), false);
    }
    true
}

/// The repo's default branch as a plain local branch name (`main`), from the shared base-ref
/// resolver — `origin/HEAD` first, so a repo whose default is `develop` is handled like any other.
fn default_branch(bare_s: &str) -> Option<String> {
    let base = bn_core::gitbase::base_ref(
        &bn_core::runner::RealRunner,
        bn_core::gitbase::BaseScope::BareRepo(Path::new(bare_s)),
    )?;
    Some(
        base.strip_prefix("origin/")
            .unwrap_or(&base)
            .trim()
            .to_string(),
    )
}

/// What to reset onto: the remote-tracking tip when it exists, else the local branch. A repo with
/// no reachable origin still gets its two checkouts agreeing with each other.
fn remote_tip(bare_s: &str, default: &str) -> String {
    let remote = format!("origin/{default}");
    let resolves = crate::git::git_ok(&[
        "--git-dir",
        bare_s,
        "rev-parse",
        "--verify",
        "--quiet",
        &remote,
    ]);
    if resolves {
        remote
    } else {
        default.to_string()
    }
}

/// The branch a checkout is on, or `None` when it is detached (or unreadable).
fn head_branch(dir_s: &str) -> Option<String> {
    crate::git::git_capture(
        &["-C", dir_s, "symbolic-ref", "--quiet", "--short", "HEAD"],
        None,
        true,
    )
    .filter(|s| !s.is_empty())
}

fn head_sha(dir_s: &str) -> Option<String> {
    crate::git::git_capture(&["-C", dir_s, "rev-parse", "HEAD"], None, true)
}

/// The **tracked** changes in a checkout, as porcelain paths. Untracked files are excluded on
/// purpose: a `reset --hard` doesn't touch them, they don't block a fast-forward or a
/// `worktree add`, and reporting them as "discarding …" would be a lie about what the job did.
fn tracked_changes(dir_s: &str) -> Vec<String> {
    if !Path::new(dir_s).is_dir() {
        return Vec::new();
    }
    // `trim: false` is load-bearing. Porcelain v1 is `XY PATH` with a FIXED two-column status, and
    // an unstaged edit's X is a space (" M .zshrc"). Trimming the whole output first ate that
    // leading space, so `get(3..)` started one byte late and every such path lost its first
    // character — `.zshrc` was reported as `zshrc`. That line is the only record of what a reset
    // is about to destroy, so it has to name the file exactly.
    crate::git::git_capture(
        &["-C", dir_s, "status", "--porcelain", "--untracked-files=no"],
        None,
        false,
    )
    .map(|out| {
        out.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.get(3..).unwrap_or(l).trim().to_string())
            .collect()
    })
    .unwrap_or_default()
}

/// How many tracked changes a checkout holds — what [`plan`] weighs, and what a reset discards.
fn dirty(dir_s: &str) -> usize {
    tracked_changes(dir_s).len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    #[test]
    fn a_missing_main_worktree_is_created() {
        assert_eq!(plan(false, None, 0, "main"), MainSync::Create);
    }

    #[test]
    fn a_clean_main_is_advanced() {
        assert_eq!(plan(true, Some("main"), 0, "main"), MainSync::Advance);
    }

    #[test]
    fn a_dirty_main_is_discarded_and_advanced() {
        // The case this job exists for: local change in a checkout nobody works in blocks the
        // fast-forward, so main rots and every branch cut from it starts stale.
        assert_eq!(plan(true, Some("main"), 5, "main"), MainSync::Discard(5));
    }

    #[test]
    fn a_main_worktree_on_another_branch_is_left_alone() {
        // Someone parked a branch here on purpose. Discarding their work to enforce a layout rule
        // would be the job doing far more damage than the drift it exists to prevent.
        assert_eq!(
            plan(true, Some("release"), 3, "main"),
            MainSync::Skip("release".to_string())
        );
        assert_eq!(
            plan(true, None, 0, "main"),
            MainSync::Skip("a detached HEAD".to_string())
        );
    }

    #[test]
    fn a_custom_default_branch_is_respected() {
        assert_eq!(plan(true, Some("develop"), 0, "develop"), MainSync::Advance);
        assert_eq!(
            plan(true, Some("main"), 0, "develop"),
            MainSync::Skip("main".to_string())
        );
    }

    /// Porcelain v1 keeps a fixed two-column status, so an unstaged edit's line starts with a
    /// SPACE. Trimming the whole output before splitting ate it and shifted every path left by
    /// one, turning `.zshrc` into `zshrc` — in the line that tells you what a reset is about to
    /// destroy, which is the worst possible place to be off by a character.
    #[test]
    fn a_dotfile_keeps_its_leading_dot_when_reported() {
        let porcelain = " M .zshrc\nM  staged.txt\n?? ignored-by-flag\n";
        let parsed: Vec<String> = porcelain
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.get(3..).unwrap_or(l).trim().to_string())
            .collect();
        assert_eq!(parsed[0], ".zshrc", "the leading dot must survive");
        assert_eq!(parsed[1], "staged.txt");
    }

    /// The same string with the old `trim: true` behaviour, to pin WHY the flag matters: the
    /// first line loses its status column and the path loses its first character.
    #[test]
    fn trimming_the_whole_output_first_is_what_broke_it() {
        let porcelain = " M .zshrc\nM  staged.txt\n";
        let first = porcelain.trim().lines().next().expect("a line");
        assert_eq!(
            first.get(3..).unwrap_or(first).trim(),
            "zshrc",
            "documents the bug: a pre-trimmed porcelain line parses one byte late"
        );
    }

    fn git_bin() -> &'static str {
        static BIN: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
        BIN.get_or_init(|| {
            [
                "/usr/bin/git",
                "/bin/git",
                "/usr/local/bin/git",
                "/opt/homebrew/bin/git",
            ]
            .into_iter()
            .find(|path| Path::new(path).exists())
            .unwrap_or("git")
        })
    }

    fn git(args: &[&str]) -> bool {
        Command::new(git_bin())
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn git_capture(args: &[&str]) -> String {
        let out = Command::new(git_bin()).args(args).output().unwrap();
        assert!(out.status.success(), "{args:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn mk_bare(default: &str) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let bare = tmp.path().join("bare").join("demo.git");
        let src_s = src.to_string_lossy().to_string();
        let bare_s = bare.to_string_lossy().to_string();
        std::fs::create_dir_all(&src).unwrap();
        assert!(git(&["init", "-q", &src_s]));
        assert!(git(&["-C", &src_s, "config", "user.email", "t@t"]));
        assert!(git(&["-C", &src_s, "config", "user.name", "t"]));
        std::fs::write(src.join("a.txt"), "hi\n").unwrap();
        assert!(git(&["-C", &src_s, "add", "-A"]));
        assert!(git(&["-C", &src_s, "commit", "-qm", "init"]));
        assert!(git(&["-C", &src_s, "branch", "-M", default]));
        std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
        assert!(git(&["clone", "-q", "--bare", &src_s, &bare_s]));
        (tmp, bare)
    }

    #[test]
    fn sync_repo_moves_a_clean_legacy_default_branch_worktree_to_main() {
        let _env = bn_core::test_support::env_lock();
        let (tmp, bare) = mk_bare("master");
        let bare_s = bare.to_string_lossy().to_string();
        let worktree_dir = tmp.path().join("worktree");
        let repo = "demo";
        let legacy = worktree_dir.join(repo).join("master");
        let legacy_s = legacy.to_string_lossy().to_string();
        let main = crate::main_slot::main_worktree(&worktree_dir, repo);
        let main_s = main.to_string_lossy().to_string();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        assert!(git(&[
            "-c",
            "safe.bareRepository=all",
            "--git-dir",
            &bare_s,
            "worktree",
            "add",
            &legacy_s,
            "master"
        ]));

        assert!(sync_repo(&bare, &worktree_dir, repo));
        assert!(crate::wt_pool::is_live_slot(&main));
        assert!(!legacy.exists());
        assert_eq!(git_capture(&["-C", &main_s, "branch", "--show-current"]), "master");
        let listing = git_capture(&["--git-dir", &bare_s, "worktree", "list", "--porcelain"]);
        assert!(listing.contains(&main_s), "{listing}");
        assert!(!listing.contains(&legacy_s), "{listing}");
    }

    #[test]
    fn sync_repo_recreates_a_broken_main_worktree() {
        let _env = bn_core::test_support::env_lock();
        let (tmp, bare) = mk_bare("main");
        let worktree_dir = tmp.path().join("worktree");
        let repo = "demo";
        let main = crate::main_slot::main_worktree(&worktree_dir, repo);
        let main_s = main.to_string_lossy().to_string();
        std::fs::create_dir_all(&main).unwrap();
        std::fs::write(main.join("stale.txt"), "broken\n").unwrap();

        assert!(sync_repo(&bare, &worktree_dir, repo));
        assert!(crate::wt_pool::is_live_slot(&main));
        assert_eq!(git_capture(&["-C", &main_s, "branch", "--show-current"]), "main");
        assert!(main.join("a.txt").exists());
    }
}
