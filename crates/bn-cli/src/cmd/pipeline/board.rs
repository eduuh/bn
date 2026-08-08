//! The cross-branch pipeline board and the tmux badge — the two read-only views over recorded
//! stages.
//!
//! Split out of `pipeline.rs` to keep it under the 1000-line ratchet (see `.file-size-baseline`).
//! They form a natural unit: neither transitions anything, both answer "what stage is this in?" —
//! one for every active worktree at once, one for the worktree under the cursor.

use super::*;

pub(super) fn active_board(cfg: &Config) -> Vec<BoardEntry> {
    let recorded: std::collections::HashMap<(String, String), BoardEntry> = stage::board()
        .into_iter()
        .map(|entry| ((entry.repo.clone(), entry.branch.clone()), entry))
        .collect();
    let mut board: Vec<BoardEntry> = rollups::all_notes(cfg)
        .into_iter()
        .map(|branch_note| {
            let event = recorded.get(&(branch_note.repo.clone(), branch_note.branch.clone()));
            let skill = stage::skill_for_note(&branch_note.note);
            BoardEntry {
                repo: branch_note.repo,
                branch: branch_note.branch,
                stage: branch_note
                    .note
                    .front
                    .stage
                    .or_else(|| event.map(|entry| entry.stage))
                    .unwrap_or(Stage::DEFAULT),
                ts: event.map(|entry| entry.ts).unwrap_or(0),
                skill,
            }
        })
        .collect();
    board.sort_by(|a, b| {
        b.ts.cmp(&a.ts)
            .then_with(|| a.repo.cmp(&b.repo))
            .then_with(|| a.branch.cmp(&b.branch))
    });
    board
}

/// `bn pipeline board` — every active local worktree's current stage, newest transition first.
pub(crate) fn emit_pipeline_board(cfg: &Config, as_json: bool) {
    let board = active_board(cfg);
    if as_json {
        println!(
            "{}",
            Envelope::success("pipeline", serde_json::json!({ "board": board })).to_json()
        );
        return;
    }
    if board.is_empty() {
        println!("pipeline board: no active local worktrees");
        return;
    }
    for e in &board {
        println!(
            "{} {:<11} {}/{}",
            e.stage.glyph(),
            e.stage.as_str(),
            e.repo,
            e.branch
        );
    }
}

/// `bn pipeline badge [path]` — the compact `glyph stage` for a worktree, for the tmux status
/// row. Resolves from `path` (tmux passes `#{pane_current_path}`) or the cwd. Legacy worktrees with
/// no recorded stage render the default pickup stage.
pub(crate) fn emit_pipeline_badge(cfg: &Config, path: Option<String>) {
    let dir = path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    if !dir.is_dir() {
        return;
    }
    let Ok(ctx) = resolver::resolve(cfg, &dir) else {
        return;
    };
    let Ok(note) = bn_core::store::read(&cfg.note_file(&ctx)) else {
        return;
    };
    let stage = stage::current_for_note(&note).unwrap_or(Stage::DEFAULT);
    print!("{} {}", stage.glyph(), stage.as_str());
}

/// Format an epoch-seconds timestamp as a compact local `MM-DD HH:MM`.
pub(super) fn fmt_ts(ts: i64) -> String {
    bn_core::util::fmt_local_short(ts)
}
