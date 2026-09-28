//! `RECAP.md`: what a seat knows at a boundary, written before the knowledge is lost.

use crate::proof::{self, ItemState};
use crate::resolver::{Config, Resolved};
use crate::store;
use std::path::PathBuf;

/// `<note_dir>/RECAP.md` — the latest recap. Older ones are archived under `recaps/`.
pub fn recap_path(cfg: &Config, ctx: &Resolved) -> PathBuf {
    cfg.note_dir(ctx).join("RECAP.md")
}

/// The recap text: the author's own words first, then a state block composed from the note —
/// goal, done-when readiness, recent progress, open todos and blockers, HEAD and uncommitted
/// files, and where the active plan is. The composed half is what a fresh context can check its
/// beliefs against.
pub fn compose(cfg: &Config, ctx: &Resolved, author: &str, stamp: &str) -> String {
    let mut out = format!("# Recap — {} / {}\n_{stamp}_\n", ctx.repo, ctx.branch);
    let author = author.trim();
    out.push_str("\n## In my words\n");
    if author.is_empty() {
        out.push_str("(none written — the state below is all that was kept)\n");
    } else {
        out.push_str(author);
        out.push('\n');
    }
    out.push_str("\n## State (composed by bn from the note — trust this over memory)\n");
    let note = store::read(&cfg.note_file(ctx)).ok();
    if let Some(n) = &note {
        if let Some(goal) = crate::render::goal_text(&n.body) {
            out.push_str(&format!("Goal: {goal}\n"));
        }
    }
    let (readiness, head) = proof::readiness_now(cfg, ctx, None);
    if !readiness.items.is_empty() {
        out.push_str("\nDone-when:\n");
        for i in &readiness.items {
            let s = match &i.state {
                ItemState::Accepted { proof, .. } => format!("accepted ({proof})"),
                ItemState::Stale { proof, .. } => format!("stale ({proof})"),
                ItemState::Attached { proofs } => {
                    format!("awaiting verdict ({})", proofs.join(", "))
                }
                ItemState::Rejected { proof, .. } => format!("rejected ({proof})"),
                ItemState::Unproven => "unproven".into(),
            };
            out.push_str(&format!("- {} {} — {s}\n", i.id, i.text));
        }
    }
    if let Some(n) = &note {
        let progress = crate::handoff::compose(n, stamp, &Default::default())
            .state
            .progress;
        if !progress.is_empty() {
            out.push_str("\nRecent progress:\n");
            for p in progress {
                out.push_str(&format!("- {p}\n"));
            }
        }
        let open: Vec<_> = n.front.todos.iter().filter(|t| t.is_open_todo()).collect();
        if !open.is_empty() {
            out.push_str("\nOpen todos:\n");
            for t in open {
                out.push_str(&format!("- [ ] {} {}\n", t.id, t.text));
            }
        }
        for b in &n.front.blockers {
            out.push_str(&format!("- ! {}\n", b.text));
        }
    }
    if let Some(sha) = &head.sha {
        out.push_str(&format!(
            "\nHEAD: {}{}\n",
            &sha[..sha.len().min(12)],
            if head.dirty {
                " (uncommitted changes)"
            } else {
                ""
            }
        ));
    }
    let plan = cfg.active_plan_file(ctx);
    if plan.exists() {
        out.push_str(&format!("Active plan: {}\n", plan.display()));
    }
    out
}

/// Write the recap to `RECAP.md` and archive a copy as `recaps/<filestamp>[-<session>].md`.
pub fn write_recap(
    cfg: &Config,
    ctx: &Resolved,
    text: &str,
    filestamp: &str,
    session: Option<&str>,
) -> std::io::Result<PathBuf> {
    let dir = cfg.note_dir(ctx);
    std::fs::create_dir_all(dir.join("recaps"))?;
    let tag = session
        .map(|s| format!("-{}", &s[..s.len().min(8)]))
        .unwrap_or_default();
    store::write_atomic(
        &dir.join("recaps").join(format!("{filestamp}{tag}.md")),
        text.as_bytes(),
    )?;
    let path = recap_path(cfg, ctx);
    store::write_atomic(&path, text.as_bytes())?;
    Ok(path)
}
