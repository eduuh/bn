//! Note-lifecycle maintenance: `prune`, `stale`, `archive`. Each scans this machine's notes
//! and reports a typed outcome; the CLI renders text or JSON. Ports of `cmd_prune`,
//! `cmd_stale`, `cmd_archive` (bn). No human printing here.

use crate::doctor::last_activity;
use crate::resolver::Config;
use crate::rollups::machine_notes;
use crate::runner::{CommandRunner, RealRunner};
use crate::store;
use crate::util::idle_days_since;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A note `prune` auto-closed because its worktree was removed.
#[derive(Debug, Serialize)]
pub struct PrunedNote {
    pub repo: String,
    pub branch: String,
}

/// Result of `prune`.
#[derive(Debug, Default, Serialize)]
pub struct PruneReport {
    pub closed: Vec<PrunedNote>,
    /// Notes whose worktree was gone but the close write failed.
    pub failed: Vec<String>,
}

/// `bn prune` — close every active note whose checked-out worktree no longer exists, so the
/// cross-branch views stop listing branches you've already cleaned up. Only worktree-managed repos
/// count: a note is considered only when `<bare_dir>/<repo>.git` exists (matched case-insensitively,
/// since the note's repo key is lowercased while the on-disk dir keeps its case).
///
/// Liveness prefers git: `git worktree list` on the bare repo is authoritative wherever a worktree
/// physically lives, so a bare-backed worktree *outside* `worktree_dir` (the arena layout) isn't
/// wrong-closed and mixed-case repos resolve. When git can't enumerate the bare (an unreadable or
/// not-actually-a-repo `.git` dir), it falls back to the original `<worktree_dir>/<repo>/<branch>`
/// path check, preserving the established behavior. The caller typically runs `archive` afterwards.
pub fn prune(cfg: &Config) -> PruneReport {
    let bare_dir = cfg.bare_dir();
    let managed: BTreeSet<String> = managed_repos(&bare_dir).into_keys().collect();
    prune_with(cfg, &managed, &live_worktrees(&bare_dir))
}

/// The decision core of [`prune`], taking the worktree-managed repo set and the git-discovered
/// live map explicitly so it's testable without real bare repos. `managed` gates which repos count;
/// `live` (populated only for repos git could enumerate) decides liveness, else the filesystem
/// path is the fallback.
fn prune_with(
    cfg: &Config,
    managed: &BTreeSet<String>,
    live: &HashMap<String, HashSet<String>>,
) -> PruneReport {
    let mut report = PruneReport::default();
    for bn in machine_notes(cfg, true) {
        if !managed.contains(&bn.repo) {
            continue; // not a worktree-managed repo → leave it
        }
        let alive = match live.get(&bn.repo) {
            Some(branches) => branches.contains(&bn.branch), // git authoritative (any location)
            None => cfg.worktree_dir.join(&bn.repo).join(&bn.branch).is_dir(), // fallback
        };
        if alive {
            continue; // still checked out
        }
        // Lock and **re-read** before flipping the status. `machine_notes` read every note up
        // front, so `bn.note` is a snapshot that may already be stale — saving it back would
        // silently revert whatever a live agent wrote in between. The note being closed here is
        // one whose worktree is gone, so contention is rare; correctness is cheap.
        let _lock = store::NoteLock::acquire(&bn.path);
        let mut note = match store::read(&bn.path) {
            Ok(n) => n,
            Err(e) => {
                report
                    .failed
                    .push(format!("{}/{}: {e}", bn.repo, bn.branch));
                continue;
            }
        };
        note.front.status = "closed".to_string();
        match store::save(&note, &bn.path) {
            Ok(()) => report.closed.push(PrunedNote {
                repo: bn.repo,
                branch: bn.branch,
            }),
            Err(e) => report
                .failed
                .push(format!("{}/{}: {e}", bn.repo, bn.branch)),
        }
    }
    report
}

/// Every repo bn manages on this machine — the worktree layout's own inventory, read from
/// `<worktree_dir>/../bare`. This is the only source that knows about a repo you have cloned but
/// not yet taken a note on, so cross-repo views seed their repo list from it rather than showing
/// only the repos that happen to have notes. Empty when the bare dir is unreadable.
pub fn cloned_repos(cfg: &Config) -> BTreeSet<String> {
    cloned_repo_dirs(cfg).into_keys().collect()
}

/// [`cloned_repos`] with each repo's git dir alongside its name — for callers that then have to
/// *read* the clone (its remote, its config). The key is lowercased to match the note-canonical repo
/// casing while the path keeps the directory's real case, so the lookup works on a case-sensitive
/// filesystem where rebuilding `<bare>/<repo>.git` from the lowercased name would miss `BigMono.git`.
pub fn cloned_repo_dirs(cfg: &Config) -> BTreeMap<String, PathBuf> {
    managed_repos(&cfg.bare_dir())
}

/// The worktree-managed repos: the lowercased stems of every `<name>.git` in `bare_dir`, mapped to
/// the dir itself. Lowercased so it matches the note's repo key even when the bare dir keeps its
/// original case (`BigMono.git`).
fn managed_repos(bare_dir: &Path) -> BTreeMap<String, PathBuf> {
    let mut map = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(bare_dir) else {
        return map;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(repo) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".git"))
            .filter(|s| !s.is_empty())
        {
            map.insert(repo.to_lowercase(), path.clone());
        }
    }
    map
}

/// Live worktrees per repo, `repo_lc → { dashed-branch }`, discovered via git. Keyed to match
/// note keys (`crate::resolver::normalize`): repo lowercased, branch slashes→dashes. Only repos
/// whose bare git enumerated successfully appear; a repo absent here (unreadable / not a real bare)
/// falls back to the filesystem check in [`prune_with`]. Every `<name>.git` is a candidate bare.
fn live_worktrees(bare_dir: &Path) -> HashMap<String, HashSet<String>> {
    let mut map: HashMap<String, HashSet<String>> = HashMap::new();
    let Ok(entries) = std::fs::read_dir(bare_dir) else {
        return map;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(repo) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".git"))
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let Some(porcelain) = git_worktree_list(&path) else {
            continue; // query failed → repo omitted, prune_with falls back to the path check
        };
        map.entry(repo.to_lowercase())
            .or_default()
            .extend(branches_from_porcelain(&porcelain));
    }
    map
}

/// The dashed branch names from `git worktree list --porcelain` output — the `branch
/// refs/heads/<b>` lines (the bare repo's own entry has no such line, so it's naturally skipped).
fn branches_from_porcelain(porcelain: &str) -> impl Iterator<Item = String> + '_ {
    porcelain
        .lines()
        .filter_map(|l| l.strip_prefix("branch refs/heads/"))
        .map(|b| b.replace('/', "-"))
}

/// Live worktree PATHS per repo, `repo_lc → { dashed-branch → worktree_path }`, discovered via git.
/// The pool-aware answer to "where is `<branch>` actually checked out": a branch dispatched into a
/// pool slot (`_pool/N`) lives there, not at the conventional `<worktree_dir>/<repo>/<branch>`, so
/// any consumer that needs the on-disk worktree (the session registry, `bn resume`) must consult
/// git rather than reconstruct the conventional path. Only repos whose bare git enumerated
/// successfully appear; a repo absent here (unreadable / not a real bare) leaves callers to their
/// own fallback. Keyed like note keys (`crate::resolver::normalize`): repo lowercased, branch
/// slashes→dashes.
pub fn live_worktree_paths(bare_dir: &Path) -> HashMap<String, HashMap<String, PathBuf>> {
    let mut map: HashMap<String, HashMap<String, PathBuf>> = HashMap::new();
    let Ok(entries) = std::fs::read_dir(bare_dir) else {
        return map;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(repo) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".git"))
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let Some(porcelain) = git_worktree_list(&path) else {
            continue;
        };
        map.insert(
            repo.to_lowercase(),
            worktree_paths_from_porcelain(&porcelain),
        );
    }
    map
}

/// Where one branch is actually checked out, asked of git rather than reconstructed.
///
/// `<worktree_dir>/<repo>/<branch>` is only a convention: a dispatched branch lives in a pool slot
/// (`_pool/N`), and a worktree directory reused for later work keeps its original name. Any caller
/// that needs a real cwd — resuming a session, running a harness turn — must consult git, or it
/// ends up running in a directory that does not exist. One `git worktree list` for the named repo,
/// so it is cheap enough for a single lookup (the whole-machine sweep is [`live_worktree_paths`]).
pub fn worktree_path(cfg: &Config, repo: &str, branch: &str) -> Option<PathBuf> {
    let bare = cfg.bare_dir().join(format!("{}.git", repo.to_lowercase()));
    let porcelain = git_worktree_list(&bare)?;
    worktree_paths_from_porcelain(&porcelain)
        .remove(&branch.replace('/', "-"))
        .filter(|p| p.is_dir())
}

/// The `dashed-branch → worktree_path` pairs from `git worktree list --porcelain`: each `worktree
/// <path>` block is followed by a `branch refs/heads/<b>` line (the bare's own entry and any
/// detached-HEAD worktree carry no such line, so they are naturally skipped — the pending
/// `worktree` path is simply overwritten by the next block). Slashes in the branch collapse to
/// dashes to match the note/worktree-dir key shape.
fn worktree_paths_from_porcelain(porcelain: &str) -> HashMap<String, PathBuf> {
    let mut map = HashMap::new();
    let mut current: Option<PathBuf> = None;
    for line in porcelain.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current = Some(PathBuf::from(path));
        } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
            if let Some(path) = current.take() {
                map.insert(branch.replace('/', "-"), path);
            }
        }
    }
    map
}

/// `git --git-dir <bare> worktree list --porcelain`. `Some(stdout)` on success — an empty-worktree
/// bare still prints its own `bare` line, so success is non-empty and distinguishable from the
/// `None` a real failure (git missing, corrupt repo) returns.
fn git_worktree_list(bare: &Path) -> Option<String> {
    let bare_s = bare.to_string_lossy();
    let out = RealRunner.git(&[
        "--git-dir",
        bare_s.as_ref(),
        "worktree",
        "list",
        "--porcelain",
    ]);
    out.success.then_some(out.stdout)
}

/// One stale note: active but untouched for at least the cutoff window.
#[derive(Debug, Serialize)]
pub struct StaleEntry {
    pub repo: String,
    pub branch: String,
    pub path: String,
    pub last_activity: String,
    pub idle_days: i64,
    pub open_todos: usize,
}

/// `bn stale [--days N]` — active notes with no activity in the last `days` days, most-idle
/// first. Unlike the oracle (which `stat`s the note file's mtime), staleness is measured from
/// the note's own last-activity date — the latest `created`/`done_at` across its items, the
/// same signal `doctor` uses. File mtimes are reset by `git checkout` in the synced note
/// repos, so they misreport; the in-note date is stable across machines and clones.
pub fn stale(cfg: &Config, today: &str, days: i64) -> Vec<StaleEntry> {
    let mut out = Vec::new();
    for bn in machine_notes(cfg, true) {
        let last = last_activity(&bn.note);
        let idle_days = idle_days_since(&last, today);
        if idle_days < days {
            continue;
        }
        let open_todos = bn
            .note
            .front
            .todos
            .iter()
            .filter(|t| t.is_open_todo())
            .count();
        out.push(StaleEntry {
            repo: bn.repo,
            branch: bn.branch,
            path: bn.path.display().to_string(),
            last_activity: last,
            idle_days,
            open_todos,
        });
    }
    out.sort_by_key(|e| std::cmp::Reverse(e.idle_days));
    out
}

/// One note `archive` moved (or, in a dry run, would move) out of the active tree.
#[derive(Debug, Serialize)]
pub struct ArchivedNote {
    pub repo: String,
    pub branch: String,
    pub created: String,
    /// Where it was moved to (the root-level `archive/<repo>/<branch>` dir).
    pub dest: String,
}

/// Result of `archive`.
#[derive(Debug, Default, Serialize)]
pub struct ArchiveReport {
    pub archived: Vec<ArchivedNote>,
    /// `<repo>/<branch>` skipped because an archive copy already exists.
    pub skipped: Vec<String>,
    pub failed: Vec<String>,
    pub dry_run: bool,
}

/// `bn archive [--days N] [--dry-run]` — relocate closed notes older than `days` (by their
/// `created` date) from the active machine subtree (`<root>/<machine>/<repo>/<branch>`) to the
/// root-level archive (`<root>/archive/<repo>/<branch>`), so cleaned-up branches stop weighing
/// on the active scans while staying recoverable. A note is archived only when its status is
/// `closed` and `created <= today - days` (string compare on `YYYY-MM-DD`). A pre-existing
/// archive copy is left in place (reported as skipped). The git commit the oracle does after
/// moving is left to the caller — this only moves files. Ports cmd_archive.
pub fn archive(cfg: &Config, today: &str, days: i64, dry_run: bool) -> ArchiveReport {
    let mut report = ArchiveReport {
        dry_run,
        ..Default::default()
    };
    let Some(cutoff) = cutoff_date(today, days) else {
        report.failed.push(format!("invalid date: {today}"));
        return report;
    };

    for bn in machine_notes(cfg, false) {
        if bn.note.front.status != "closed" {
            continue;
        }
        let created = bn.note.front.created.clone();
        if created.is_empty() || created.as_str() > cutoff.as_str() {
            continue; // missing date or too recent
        }

        let dest = cfg
            .notes_dir_for(&bn.repo)
            .join("archive")
            .join(&bn.repo)
            .join(&bn.branch);
        if dest.exists() {
            report.skipped.push(format!("{}/{}", bn.repo, bn.branch));
            continue;
        }

        let entry = ArchivedNote {
            repo: bn.repo.clone(),
            branch: bn.branch.clone(),
            created,
            dest: dest.display().to_string(),
        };
        if dry_run {
            report.archived.push(entry);
            continue;
        }

        let src_dir = match bn.path.parent() {
            Some(p) => p.to_path_buf(),
            None => {
                report
                    .failed
                    .push(format!("{}/{}: no parent", bn.repo, bn.branch));
                continue;
            }
        };
        if let Some(parent) = dest.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                report
                    .failed
                    .push(format!("{}/{}: mkdir: {e}", bn.repo, bn.branch));
                continue;
            }
        }
        match std::fs::rename(&src_dir, &dest) {
            Ok(()) => {
                prune_empty_dir(src_dir.parent()); // drop the now-empty <machine>/<repo> dir
                report.archived.push(entry);
            }
            Err(e) => report
                .failed
                .push(format!("{}/{}: {e}", bn.repo, bn.branch)),
        }
    }
    report
}

/// `today - days` as `YYYY-MM-DD`, or `None` if `today` doesn't parse. Pure civil-date math.
fn cutoff_date(today: &str, days: i64) -> Option<String> {
    let d = chrono::NaiveDate::parse_from_str(today.trim(), "%Y-%m-%d").ok()?;
    Some(
        (d - chrono::Duration::days(days))
            .format("%Y-%m-%d")
            .to_string(),
    )
}

/// How many empty-text items `clean` removed from one note.
#[derive(Debug, Serialize)]
pub struct CleanEntry {
    pub repo: String,
    pub branch: String,
    pub removed: usize,
}

/// `bn clean` — drop empty-text (whitespace-only) items from every section of the current
/// note. In the single-file model the real data lives in frontmatter and the oracle's blank
/// `- [ ]`/`-` lines are render placeholders (regenerated on save), so the meaningful analogue
/// is removing stray empty items a manual frontmatter edit may have left behind. Returns the
/// number removed; `NoNote` if the note doesn't exist. Writes only when something was removed.
pub fn clean(
    cfg: &Config,
    ctx: &crate::resolver::Resolved,
) -> Result<usize, crate::commands::CmdError> {
    use crate::commands::CmdError;
    let note_md = cfg.note_file(ctx);
    if !note_md.exists() {
        return Err(CmdError::NoNote);
    }
    // Locked read-modify-write, like every `commands::*` verb — otherwise a `bn clean` racing a
    // live agent's `bn add` writes back a note that predates the add.
    let _lock = store::NoteLock::acquire(&note_md);
    let mut note = store::read(&note_md)?;
    let removed = strip_empty_items(&mut note);
    if removed > 0 {
        store::save(&note, &note_md)?;
    }
    Ok(removed)
}

/// `bn clean --all` — run `clean` over every active note. Only notes that actually had empty
/// items are returned (removed > 0), in oracle scan order.
pub fn clean_all(cfg: &Config) -> Vec<CleanEntry> {
    let mut out = Vec::new();
    for bn in machine_notes(cfg, true) {
        // Re-read under the lock rather than saving the up-front `machine_notes` snapshot back —
        // a sweep across every note is exactly where a stale write is most likely to land on a
        // note an agent is actively editing.
        let _lock = store::NoteLock::acquire(&bn.path);
        let Ok(mut note) = store::read(&bn.path) else {
            continue;
        };
        let removed = strip_empty_items(&mut note);
        if removed > 0 && store::save(&note, &bn.path).is_ok() {
            out.push(CleanEntry {
                repo: bn.repo,
                branch: bn.branch,
                removed,
            });
        }
    }
    out
}

/// Remove whitespace-only items from all six sections of a note; return how many were dropped.
fn strip_empty_items(note: &mut crate::model::Note) -> usize {
    let mut removed = 0;
    for section in crate::model::Section::all() {
        let items = note.front.items_mut(section);
        let before = items.len();
        items.retain(|it| !it.text.trim().is_empty());
        removed += before - items.len();
    }
    removed
}

/// Best-effort `rmdir` of a now-empty `<machine>/<repo>` dir after its last branch moved out.
/// Silent on any error (non-empty, missing, permission) — leaving the dir is harmless.
fn prune_empty_dir(dir: Option<&std::path::Path>) {
    if let Some(d) = dir {
        if std::fs::read_dir(d)
            .map(|mut e| e.next().is_none())
            .unwrap_or(false)
        {
            // Best-effort (see fn doc): leaving the dir on any error is harmless.
            let _ = std::fs::remove_dir(d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{add, set_status};
    use crate::resolver::Resolved;
    use std::path::Path;

    fn cfg(root: &Path, worktree: &Path) -> Config {
        Config {
            personal_root: root.to_path_buf(),
            work_root: root.to_path_buf(),
            worktree_dir: worktree.to_path_buf(),
            hostname: "testhost".into(),
            ..Default::default()
        }
    }

    fn ctx(repo: &str, branch: &str) -> Resolved {
        Resolved {
            repo: repo.into(),
            branch: branch.into(),
        }
    }

    fn note_status(c: &Config, x: &Resolved) -> String {
        store::read(&c.note_file(x)).unwrap().front.status
    }

    #[test]
    fn clean_removes_empty_items_only() {
        use crate::model::Item;
        let dir = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let c = cfg(dir.path(), wt.path());
        let x = ctx("acme", "main");
        add(&c, &x, "todo", "real todo", None, "2026-06-20").unwrap();
        add(&c, &x, "blocker", "real blocker", None, "2026-06-20").unwrap();
        // inject two empty-text items by hand (manual-edit analogue)
        let mut note = store::read(&c.note_file(&x)).unwrap();
        note.front.todos.push(Item {
            id: "t9".into(),
            text: "   ".into(),
            created: "2026-06-20".into(),
            done: Some(false),
            done_at: None,
            link: None,
            pct: None,
            est: None,
            actual: None,
            q: None,
            notes: Vec::new(),
        });
        note.front.decisions.push(Item {
            id: "d9".into(),
            text: "".into(),
            created: "2026-06-20".into(),
            done: None,
            link: None,
            pct: None,
            q: None,
            done_at: None,
            est: None,
            actual: None,
            notes: Vec::new(),
        });
        store::save(&note, &c.note_file(&x)).unwrap();

        let removed = clean(&c, &x).unwrap();
        assert_eq!(removed, 2);
        let after = store::read(&c.note_file(&x)).unwrap();
        assert_eq!(after.front.todos.len(), 1);
        assert_eq!(after.front.todos[0].text, "real todo");
        assert!(after.front.decisions.is_empty());
        // idempotent: second run removes nothing
        assert_eq!(clean(&c, &x).unwrap(), 0);
        // clean_all reports nothing once clean
        assert!(clean_all(&c).is_empty());
        // missing note errors
        assert_eq!(
            clean(&c, &ctx("nope", "main")).unwrap_err().code(),
            "no_note"
        );
    }

    #[test]
    fn branches_from_porcelain_extracts_and_dashes_branches() {
        // A real bare-repo listing: the bare's own entry has no `branch` line; a worktree living
        // outside worktree_dir and a slashed branch both come through, dashed.
        let porcelain = "\
worktree /projects/bare/bn.git
bare

worktree /anywhere/main
HEAD abc
branch refs/heads/main

worktree /anywhere/featx
HEAD def
branch refs/heads/feat/x
";
        let got: Vec<String> = branches_from_porcelain(porcelain).collect();
        assert_eq!(got, vec!["main".to_string(), "feat-x".to_string()]);
    }

    #[test]
    fn worktree_paths_from_porcelain_pairs_pool_slots_and_dashes_branches() {
        // The bare's own entry and a detached-HEAD pool slot carry no `branch` line, so both are
        // skipped; a branch dispatched into a pool slot (`_pool/2`) resolves to that slot's path,
        // not the conventional `<worktree_dir>/<repo>/<branch>`; a slashed branch collapses to dashes.
        let porcelain = "\
worktree /projects/bare/bn.git
bare

worktree /projects/worktree/bn/_pool/2
HEAD e0730c0
branch refs/heads/bn-resume

worktree /projects/worktree/bn/_pool/5
HEAD 6529095
detached

worktree /projects/worktree/bn/feat-x
HEAD def
branch refs/heads/feat/x
";
        let got = worktree_paths_from_porcelain(porcelain);
        assert_eq!(
            got.get("bn-resume"),
            Some(&PathBuf::from("/projects/worktree/bn/_pool/2"))
        );
        assert_eq!(
            got.get("feat-x"),
            Some(&PathBuf::from("/projects/worktree/bn/feat-x"))
        );
        // The bare and the detached-HEAD slot contribute no entries.
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn prune_closes_only_managed_repos_with_no_live_worktree() {
        let proj = tempfile::tempdir().unwrap();
        let notes = proj.path().join("notes");
        let worktree = proj.path().join("worktree");
        let c = cfg(&notes, &worktree);

        // `bn`: git enumerated it (in `live`); only `main` has a live worktree.
        add(&c, &ctx("bn", "feat"), "todo", "x", None, "2026-06-20").unwrap();
        add(&c, &ctx("bn", "main"), "todo", "x", None, "2026-06-20").unwrap();
        // `fake`: managed but git couldn't enumerate (absent from `live`) → filesystem fallback.
        add(&c, &ctx("fake", "gone"), "todo", "x", None, "2026-06-20").unwrap();
        add(&c, &ctx("fake", "live"), "todo", "x", None, "2026-06-20").unwrap();
        std::fs::create_dir_all(worktree.join("fake").join("live")).unwrap(); // path fallback = alive
                                                                              // `solo`: not worktree-managed at all → never pruned.
        add(&c, &ctx("solo", "main"), "todo", "x", None, "2026-06-20").unwrap();

        let managed = BTreeSet::from(["bn".to_string(), "fake".to_string()]);
        let mut live: HashMap<String, HashSet<String>> = HashMap::new();
        live.insert("bn".to_string(), HashSet::from(["main".to_string()]));

        let r = prune_with(&c, &managed, &live);
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        let mut closed: Vec<_> = r
            .closed
            .iter()
            .map(|p| (p.repo.clone(), p.branch.clone()))
            .collect();
        closed.sort();
        assert_eq!(
            closed,
            vec![
                ("bn".to_string(), "feat".to_string()),
                ("fake".to_string(), "gone".to_string()),
            ]
        );
        assert_eq!(note_status(&c, &ctx("bn", "feat")), "closed"); // git: branch absent
        assert_eq!(note_status(&c, &ctx("bn", "main")), "active"); // git: branch live
        assert_eq!(note_status(&c, &ctx("fake", "gone")), "closed"); // fallback: no path
        assert_eq!(note_status(&c, &ctx("fake", "live")), "active"); // fallback: path exists
        assert_eq!(note_status(&c, &ctx("solo", "main")), "active"); // unmanaged
    }

    #[test]
    fn prune_with_end_to_end_over_real_bare_repos() {
        // Exercises live_worktrees()/prune() against real git bares, including a worktree that
        // lives OUTSIDE worktree_dir and a mixed-case repo — the two cases the old path-guess broke.
        let proj = tempfile::tempdir().unwrap();
        let notes = proj.path().join("notes");
        let bare_dir = proj.path().join("bare");
        // bare_dir must be a sibling of worktree_dir named "bare" (see Config::bare_dir()).
        let c = cfg(&notes, &proj.path().join("worktree"));
        std::fs::create_dir_all(&bare_dir).unwrap();

        let g = |args: &[&str]| {
            assert!(RealRunner.git(args).success);
        };
        // A mixed-case bare repo with a seeded main commit.
        let bare = bare_dir.join("BigMono.git");
        g(&["init", "-q", "--bare", bare.to_str().unwrap()]);
        let seed = proj.path().join("seed");
        g(&[
            "clone",
            "-q",
            bare.to_str().unwrap(),
            seed.to_str().unwrap(),
        ]);
        // Supply an identity inline: CI runners have no global git user configured.
        g(&[
            "-C",
            seed.to_str().unwrap(),
            "-c",
            "user.email=t@t.co",
            "-c",
            "user.name=t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ]);
        g(&[
            "-C",
            seed.to_str().unwrap(),
            "push",
            "-q",
            "origin",
            "HEAD:main",
        ]);
        // A live worktree for `feat/x`, deliberately OUTSIDE worktree_dir.
        let elsewhere = proj.path().join("elsewhere").join("featx");
        g(&[
            "--git-dir",
            bare.to_str().unwrap(),
            "worktree",
            "add",
            "-q",
            "-b",
            "feat/x",
            elsewhere.to_str().unwrap(),
            "main",
        ]);

        // Note for the live out-of-tree branch (repo lowercased, branch dashed) → kept active.
        add(&c, &ctx("bigmono", "feat-x"), "todo", "x", None, "2026-06-20").unwrap();
        // Note for a branch with no worktree → pruned.
        add(&c, &ctx("bigmono", "gone"), "todo", "x", None, "2026-06-20").unwrap();

        let r = prune(&c);
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        assert_eq!(note_status(&c, &ctx("bigmono", "feat-x")), "active");
        assert_eq!(note_status(&c, &ctx("bigmono", "gone")), "closed");
    }

    #[test]
    fn stale_lists_inactive_notes_most_idle_first() {
        let dir = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let c = cfg(dir.path(), wt.path());

        // old: created long ago, no later activity → idle
        add(&c, &ctx("alpha", "main"), "todo", "old", None, "2026-01-01").unwrap();
        // older still
        add(
            &c,
            &ctx("beta", "main"),
            "todo",
            "older",
            None,
            "2025-06-01",
        )
        .unwrap();
        // fresh: activity today → not stale
        add(
            &c,
            &ctx("gamma", "main"),
            "todo",
            "fresh",
            None,
            "2026-06-20",
        )
        .unwrap();

        let s = stale(&c, "2026-06-20", 14);
        let repos: Vec<_> = s.iter().map(|e| e.repo.as_str()).collect();
        assert_eq!(
            repos,
            vec!["beta", "alpha"],
            "most-idle first, fresh excluded"
        );
        assert_eq!(s[0].open_todos, 1);
        assert!(s[0].idle_days > s[1].idle_days);
    }

    #[test]
    fn stale_excludes_closed_notes() {
        let dir = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let c = cfg(dir.path(), wt.path());
        add(&c, &ctx("alpha", "main"), "todo", "old", None, "2026-01-01").unwrap();
        set_status(&c, &ctx("alpha", "main"), "closed").unwrap();
        assert!(stale(&c, "2026-06-20", 14).is_empty());
    }

    #[test]
    fn archive_moves_old_closed_notes_to_root_archive() {
        let dir = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let c = cfg(dir.path(), wt.path());

        // old + closed → archived
        add(&c, &ctx("alpha", "main"), "todo", "x", None, "2026-01-01").unwrap();
        set_status(&c, &ctx("alpha", "main"), "closed").unwrap();
        // closed but recent → kept active-tree
        add(&c, &ctx("beta", "main"), "todo", "x", None, "2026-06-15").unwrap();
        set_status(&c, &ctx("beta", "main"), "closed").unwrap();
        // old but still active → not archived
        add(&c, &ctx("gamma", "main"), "todo", "x", None, "2026-01-01").unwrap();

        let r = archive(&c, "2026-06-20", 30, false);
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        let archived: Vec<_> = r.archived.iter().map(|a| a.repo.as_str()).collect();
        assert_eq!(archived, vec!["alpha"]);

        // moved to root-level archive, removed from the machine subtree
        let archived_note = dir
            .path()
            .join("archive")
            .join("alpha")
            .join("main")
            .join("note.md");
        assert!(archived_note.exists());
        assert!(!dir
            .path()
            .join("testhost")
            .join("alpha")
            .join("main")
            .exists());
        // empty <machine>/alpha dir cleaned up
        assert!(!dir.path().join("testhost").join("alpha").exists());
        // untouched
        assert!(dir
            .path()
            .join("testhost")
            .join("beta")
            .join("main")
            .exists());
        assert!(dir
            .path()
            .join("testhost")
            .join("gamma")
            .join("main")
            .exists());
    }

    #[test]
    fn archive_dry_run_touches_nothing_and_skips_existing() {
        let dir = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let c = cfg(dir.path(), wt.path());
        add(&c, &ctx("alpha", "main"), "todo", "x", None, "2026-01-01").unwrap();
        set_status(&c, &ctx("alpha", "main"), "closed").unwrap();

        // dry run reports but moves nothing
        let r = archive(&c, "2026-06-20", 30, true);
        assert_eq!(r.archived.len(), 1);
        assert!(r.dry_run);
        assert!(dir
            .path()
            .join("testhost")
            .join("alpha")
            .join("main")
            .exists());

        // pre-existing archive copy → skipped, source left in place
        std::fs::create_dir_all(dir.path().join("archive").join("alpha").join("main")).unwrap();
        let r = archive(&c, "2026-06-20", 30, false);
        assert!(r.archived.is_empty());
        assert_eq!(r.skipped, vec!["alpha/main".to_string()]);
        assert!(dir
            .path()
            .join("testhost")
            .join("alpha")
            .join("main")
            .exists());
    }
}
