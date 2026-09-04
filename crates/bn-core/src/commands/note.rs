//! Note-lifecycle verbs: create the note, set its status, close it, and set the
//! PR / work-item / goal metadata.

use super::error::{emit_metric, CmdError};
use crate::model::{Note, Scalar};
use crate::resolver::{Config, Resolved};
use crate::store::{self, StoreError};
use serde_json::json;

/// Result of `set_goal`: the stored goal line.
#[derive(Debug)]
pub struct GoalOutcome {
    pub goal: String,
}

/// Ensure the branch note exists, creating an empty single-file note if missing.
/// Returns the note directory (what bare `bn` prints).
///
/// Locked, and the existence check is repeated *inside* the lock. Unlocked, this was a
/// check-then-act: two processes could both see no note, and the loser would write a blank
/// `Note::new` over the goal and todos the winner had just seeded. `bn dispatch` calls this first
/// thing on every dispatch, so two dispatches landing together on one branch — a re-dispatch, an
/// orchestrator retry — could silently erase the note they had come to create.
pub fn ensure_note(
    cfg: &Config,
    ctx: &Resolved,
    today: &str,
) -> Result<std::path::PathBuf, CmdError> {
    let note_md = cfg.note_file(ctx);
    if !note_md.exists() {
        let _lock = store::NoteLock::acquire(&note_md);
        // Re-check under the lock: another writer may have created (and seeded) it while we waited.
        if !note_md.exists() {
            let mut note = Note::new(ctx.rb(), today, &cfg.hostname);
            note.front.record_session();
            store::save(&note, &note_md)?;
        }
    }
    Ok(cfg.note_dir(ctx))
}

/// Set the note's status (`closed` / `active`). Errors if the note doesn't exist.
pub fn set_status(cfg: &Config, ctx: &Resolved, status: &str) -> Result<(), CmdError> {
    let note_md = cfg.note_file(ctx);
    if !note_md.exists() {
        return Err(CmdError::NoNote);
    }
    // Lock the read-modify-write (see set_goal) so a parallel writer can't lose this update.
    let _lock = store::NoteLock::acquire(&note_md);
    let mut note = store::read(&note_md)?;
    note.front.status = status.to_string();
    store::save(&note, &note_md)?;
    Ok(())
}

/// What `close` completed: the note it closed, how many open todos it marked done, and the
/// achievement line it logged (if any).
#[derive(Debug)]
pub struct CloseOutcome {
    pub repo: String,
    pub branch: String,
    pub todos_completed: usize,
    pub achievement: Option<String>,
    /// Acceptance criteria (`## Done When`) still unmet at close — a warning, not a block. Empty
    /// when the branch declared none or met them all.
    pub unmet_criteria: Vec<String>,
    /// The note's PR/work-item links that are not in the `bn links` store — a warning, not a
    /// block. Closing is the last moment these are cheap to keep: the note is about to stop
    /// being the thing anyone reads, and the worktree is usually next. Empty when the branch
    /// linked neither, or when both are already stored.
    pub uncaptured_links: Vec<String>,
}

/// Complete and close the branch note: mark every open todo done (stamped `today`), set the
/// status to `closed`, and log a one-line achievement to `<note_dir>/achievements.md`. The
/// achievement text is `achievement` when given, otherwise derived from the note's Goal (plus
/// its PR); nothing is logged when neither yields text. Errors if the note doesn't exist.
/// Re-closing an already-closed note is a no-op beyond re-stamping (idempotent enough for the
/// `wt cleanup` path that may run twice).
pub fn close(
    cfg: &Config,
    ctx: &Resolved,
    today: &str,
    achievement: Option<&str>,
) -> Result<CloseOutcome, CmdError> {
    let note_md = cfg.note_file(ctx);
    if !note_md.exists() {
        return Err(CmdError::NoNote);
    }
    // Lock the read-modify-write (see set_goal) so a parallel writer can't lose this update.
    let _lock = store::NoteLock::acquire(&note_md);
    let mut note = store::read(&note_md)?;

    // Idempotent re-close: closing an already-closed note (e.g. `bn close` then the cleanup it
    // triggers calling close again, or a double cleanup pass) must not re-emit the close-out
    // metric or re-append the achievement — that would double-count. Todos are already done, so
    // nothing here changes the note; we still re-save to keep the call total, but skip the signals.
    let was_closed = note.front.status == "closed";

    let mut todos_completed = 0;
    for t in note.front.todos.iter_mut() {
        if t.is_open_todo() {
            t.done = Some(true);
            t.done_at = Some(today.to_string());
            todos_completed += 1;
        }
    }
    // Acceptance criteria are never auto-completed — that's the point of warning about them.
    // Collect any still unmet so the caller can surface "closed, but N criteria weren't met".
    let unmet_criteria: Vec<String> = note
        .front
        .done_when
        .iter()
        .filter(|c| c.done == Some(false))
        .map(|c| c.text.clone())
        .collect();

    note.front.status = "closed".to_string();
    store::save(&note, &note_md)?;

    // The close-out signal: pairs with this branch's pr_merged event to measure how long work
    // lingered after it landed (the literal "time to close out work"). PR carried for matching.
    if !was_closed {
        let mut extra = vec![("todos_completed", json!(todos_completed))];
        let pr_str = note.front.pr.as_ref().map(Scalar::display);
        if let Some(ref p) = pr_str {
            extra.push(("pr", json!(p)));
        }
        emit_metric(cfg, ctx, "note_closed", extra);
    }

    let text = achievement
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| derived_achievement(&note));
    if !was_closed {
        if let Some(ref t) = text {
            append_achievement(cfg, ctx, today, t)?;
        }
    }

    Ok(CloseOutcome {
        repo: ctx.repo.clone(),
        branch: ctx.branch.clone(),
        todos_completed,
        // A re-close logged nothing — don't report an achievement it didn't append.
        achievement: if was_closed { None } else { text },
        unmet_criteria,
        uncaptured_links: uncaptured_links(cfg, ctx, &note),
    })
}

/// The note's PR and work item that the `bn links` store does not already hold.
///
/// Reads the same [`crate::links::capturable`] that `bn links capture` writes from, so the
/// warning names exactly what that command would store — including a bare work-item id, which
/// resolves through the repo's forge rather than being skipped.
///
/// Read-only and best-effort: an unreadable store yields "nothing captured", which over-warns
/// rather than under-warns. Closing a note must never fail over a bookmark list.
fn uncaptured_links(cfg: &Config, ctx: &Resolved, note: &Note) -> Vec<String> {
    let stored = crate::links::read(cfg);
    let pr = note.front.pr.as_ref().map(Scalar::display);
    let wi = note.front.work_item.as_ref().map(Scalar::display);
    crate::links::capturable(cfg, &ctx.repo, pr.as_deref(), wi.as_deref())
        .into_iter()
        .map(|c| c.url)
        .filter(|url| !stored.iter().any(|l| &l.url == url))
        .collect()
}

/// Default achievement line when none is given: the note's Goal, suffixed with its PR when
/// present (`<goal> (<pr>)`). `None` when the Goal is empty/absent — then nothing is logged.
fn derived_achievement(note: &Note) -> Option<String> {
    let goal = crate::render::goal_text(&note.body)?;
    let goal = goal.trim();
    if goal.is_empty() {
        return None;
    }
    match &note.front.pr {
        Some(pr) => Some(format!("{goal} ({})", pr.display())),
        None => Some(goal.to_string()),
    }
}

/// Append a dated bullet to the note's `achievements.md`, creating it (with a header) if
/// absent. Newest entries append at the end — a simple running log.
fn append_achievement(
    cfg: &Config,
    ctx: &Resolved,
    today: &str,
    text: &str,
) -> Result<(), StoreError> {
    let path = cfg.note_dir(ctx).join("achievements.md");
    let mut content = std::fs::read_to_string(&path).unwrap_or_default();
    if content.trim().is_empty() {
        content = "# Achievements\n\n".to_string();
    } else if !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(&format!("- {today} — {text}\n"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, content)?;
    Ok(())
}

/// Result of `set_pr`: the stored PR value (URL or number, kept as a string).
#[derive(Debug)]
pub struct PrOutcome {
    pub pr: String,
}

/// Set the branch note's `pr` field. Creates an empty note if none exists yet, mirroring
/// `add`, so an agent can record the PR the moment it opens one.
pub fn set_pr(cfg: &Config, ctx: &Resolved, pr: &str, today: &str) -> Result<PrOutcome, CmdError> {
    let pr = pr.trim();
    if pr.is_empty() {
        return Err(CmdError::EmptyText);
    }
    let note_md = cfg.note_file(ctx);
    // Lock the read-modify-write (see set_goal) so a parallel writer can't lose this update.
    let _lock = store::NoteLock::acquire(&note_md);
    let mut note = if note_md.exists() {
        store::read(&note_md)?
    } else {
        Note::new(ctx.rb(), today, &cfg.hostname)
    };
    note.front.pr = Some(Scalar::Str(pr.to_string()));
    note.front.record_session();
    store::save(&note, &note_md)?;
    emit_metric(
        cfg,
        ctx,
        "activity",
        vec![("action", json!("pr")), ("pr", json!(pr))],
    );
    Ok(PrOutcome { pr: pr.to_string() })
}

/// Result of `set_work_item`: the stored work-item id (as given).
#[derive(Debug)]
pub struct WorkItemOutcome {
    pub work_item: String,
}

/// Set the branch note's `work_item` field. Like `set_pr`, creates an empty note if none
/// exists yet. A numeric id is stored as a YAML number (round-trips with the store), other
/// ids as strings. Unlike the oracle, no duplicate `## Collaboration` line is appended — the
/// frontmatter field is the single source of truth (consistent with `set_pr`).
pub fn set_work_item(
    cfg: &Config,
    ctx: &Resolved,
    id: &str,
    today: &str,
) -> Result<WorkItemOutcome, CmdError> {
    let id = id.trim();
    if id.is_empty() {
        return Err(CmdError::EmptyText);
    }
    let note_md = cfg.note_file(ctx);
    // Lock the read-modify-write (see set_goal) so a parallel writer can't lose this update.
    let _lock = store::NoteLock::acquire(&note_md);
    let mut note = if note_md.exists() {
        store::read(&note_md)?
    } else {
        Note::new(ctx.rb(), today, &cfg.hostname)
    };
    note.front.work_item = Some(Scalar::parse(id));
    note.front.record_session();
    store::save(&note, &note_md)?;
    Ok(WorkItemOutcome {
        work_item: id.to_string(),
    })
}

/// Read the stored PR value for a note, or `None` if there's no note or no PR set.
pub fn read_pr(cfg: &Config, ctx: &Resolved) -> Option<String> {
    let note = store::read(&cfg.note_file(ctx)).ok()?;
    note.front.pr.as_ref().map(Scalar::display)
}

/// Read the stored work-item id for a note, or `None` if there's no note or none set.
pub fn read_work_item(cfg: &Config, ctx: &Resolved) -> Option<String> {
    let note = store::read(&cfg.note_file(ctx)).ok()?;
    note.front.work_item.as_ref().map(Scalar::display)
}

/// Set the note's `## Goal` line. Creates an empty note if none exists yet, mirroring
/// `set_pr`, so the agent can record the goal as its first action in a fresh worktree.
pub fn set_goal(
    cfg: &Config,
    ctx: &Resolved,
    goal: &str,
    today: &str,
) -> Result<GoalOutcome, CmdError> {
    let goal = goal.trim();
    if goal.is_empty() {
        return Err(CmdError::EmptyText);
    }
    let note_md = cfg.note_file(ctx);
    // Guard the whole read-modify-write so a parallel writer (e.g. the same worktree's agent)
    // can't interleave and lose this update. Atomic save handles corruption; this handles races.
    let _lock = store::NoteLock::acquire(&note_md);
    let mut note = if note_md.exists() {
        store::read(&note_md)?
    } else {
        Note::new(ctx.rb(), today, &cfg.hostname)
    };
    note.body = crate::render::set_goal_in_body(&note.body, goal);
    note.front.record_session();
    store::save(&note, &note_md)?;
    emit_metric(cfg, ctx, "activity", vec![("action", json!("goal"))]);
    Ok(GoalOutcome {
        goal: goal.to_string(),
    })
}
