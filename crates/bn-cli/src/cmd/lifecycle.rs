//! `bn clean`/`bn prune`/`bn stale`/`bn archive` — note lifecycle maintenance.
use crate::*;

/// `bn clean` — strip empty-text placeholder items from the current note.
pub(crate) fn emit_clean(cfg: &Config, ctx: &Resolved, as_json: bool) {
    match bn_core::lifecycle::clean(cfg, ctx) {
        Ok(removed) => {
            if as_json {
                let entries = if removed > 0 {
                    vec![bn_core::lifecycle::CleanEntry {
                        repo: ctx.repo.clone(),
                        branch: ctx.branch.clone(),
                        removed,
                    }]
                } else {
                    vec![]
                };
                println!(
                    "{}",
                    Envelope::success("clean", payload::clean_data(&entries)).to_json()
                );
            } else if removed == 0 {
                println!("No placeholders in {}/{}", ctx.repo, ctx.branch);
            } else {
                println!(
                    "{}/{}: removed {removed} placeholder(s)",
                    ctx.repo, ctx.branch
                );
            }
        }
        Err(e) => fail(&e.message()),
    }
}

/// `bn clean --all` — strip empty-text items from every active note.
pub(crate) fn emit_clean_all(cfg: &Config, as_json: bool) {
    let entries = bn_core::lifecycle::clean_all(cfg);
    if as_json {
        println!(
            "{}",
            Envelope::success("clean", payload::clean_data(&entries)).to_json()
        );
        return;
    }
    if entries.is_empty() {
        println!("No placeholders found");
        return;
    }
    let mut total = 0;
    for e in &entries {
        println!(
            "{}/{}: removed {} placeholder(s)",
            e.repo, e.branch, e.removed
        );
        total += e.removed;
    }
    println!("\nTotal removed: {total}");
}

/// `bn prune` — close notes whose worktree is gone, then auto-archive old closed notes
/// (the oracle couples these two: cmd_prune ends by calling cmd_archive).
pub(crate) fn emit_prune(cfg: &Config, as_json: bool) {
    let report = bn_core::lifecycle::prune(cfg);
    if as_json {
        println!(
            "{}",
            Envelope::success("prune", payload::prune_data(&report)).to_json()
        );
        // Still run the coupled archive so the JSON caller gets the same side effects.
        let arch = bn_core::lifecycle::archive(cfg, &today(), 30, false);
        commit_archived(cfg, &arch);
        return;
    }
    for f in &report.failed {
        eprintln!("prune failed: {f}");
    }
    if report.closed.is_empty() {
        println!("Nothing to prune");
    } else {
        for p in &report.closed {
            println!("Closed: {}/{} (worktree removed)", p.repo, p.branch);
        }
    }
    run_archive(cfg, 30, false, false); // auto-archive, like the oracle
}

pub(crate) fn emit_stale(cfg: &Config, days: i64, as_json: bool) {
    let entries = bn_core::lifecycle::stale(cfg, &today(), days);
    if as_json {
        println!(
            "{}",
            Envelope::success("stale", payload::stale_data(&entries)).to_json()
        );
        return;
    }
    if entries.is_empty() {
        println!("No stale notes (all modified within {days} days)");
        return;
    }
    for e in &entries {
        println!(
            "{}/{}  last activity: {}  ({} open todos, idle {}d)",
            e.repo, e.branch, e.last_activity, e.open_todos, e.idle_days
        );
    }
}

pub(crate) fn emit_archive(cfg: &Config, days: i64, dry_run: bool, as_json: bool) {
    // `archive` is not idempotent (it moves files), so call it exactly once per invocation:
    // the JSON path here, the text path inside `run_archive`.
    if as_json {
        let report = bn_core::lifecycle::archive(cfg, &today(), days, dry_run);
        println!(
            "{}",
            Envelope::success("archive", payload::archive_data(&report)).to_json()
        );
        if !dry_run {
            commit_archived(cfg, &report);
        }
        return;
    }
    run_archive(cfg, days, dry_run, true);
}

/// Run archive and print human output. `verbose` controls the leading "Archive: …" lines (on
/// for `bn archive`, off for the auto-archive tail of `bn prune` which already printed closes).
pub(crate) fn run_archive(cfg: &Config, days: i64, dry_run: bool, verbose: bool) {
    let report = bn_core::lifecycle::archive(cfg, &today(), days, dry_run);
    if dry_run {
        println!("[DRY RUN — no changes will be made]");
    }
    for f in &report.failed {
        eprintln!("archive failed: {f}");
    }
    if verbose {
        for a in &report.archived {
            println!("Archive: {}/{} (created {})", a.repo, a.branch, a.created);
        }
        for s in &report.skipped {
            eprintln!("Already archived, skipping: {s}");
        }
    }
    let verb = if dry_run { "Would archive" } else { "Archived" };
    println!("{verb}: {} note(s)", report.archived.len());
    if !dry_run {
        commit_archived(cfg, &report);
    }
}

/// Best-effort, surgical commit of the just-archived notes in each notes root (mirrors
/// cmd_archive's per-dir commit). Only the specific moved paths are staged — never `git add
/// -A` — so an unrelated in-flight edit in the real notes repo is never swept into the commit.
/// The move already happened on disk; a failed git step just leaves it uncommitted for the
/// next refresh/sync to pick up, so errors are swallowed.
pub(crate) fn commit_archived(cfg: &Config, report: &bn_core::lifecycle::ArchiveReport) {
    if report.archived.is_empty() {
        return;
    }
    let mut roots = vec![cfg.personal_root.clone(), cfg.work_root.clone()];
    roots.dedup();
    for root in roots {
        if !root.join(".git").exists() {
            continue;
        }
        let root_s = root.display().to_string();
        // Stage each moved note's destination (the add) and source (the removal), relative to
        // the root. `git add -A -- <pathspec>` records both the new file and the deletion.
        let mut staged_any = false;
        for a in &report.archived {
            let dest = std::path::Path::new(&a.dest);
            let src = cfg
                .notes_dir_for(&a.repo)
                .join(cfg.machine())
                .join(&a.repo)
                .join(&a.branch);
            for p in [dest.to_path_buf(), src] {
                if let Ok(rel) = p.strip_prefix(&root) {
                    let rel_s = rel.display().to_string();
                    let ok = crate::git::git_run(&["-C", &root_s, "add", "-A", "--", &rel_s]);
                    staged_any |= ok;
                }
            }
        }
        if !staged_any {
            continue;
        }
        let dirty = !crate::git::git_run(&["-C", &root_s, "diff", "--cached", "--quiet"]);
        if dirty {
            let _ = crate::git::git_run(&["-C", &root_s, "commit", "-m", "archive notes"]);
        }
    }
}
