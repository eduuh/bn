//! Proof applied: readiness at a branch's real head, and what `bn done w<N>` does about it.

use super::{derive, list, verdicts, Freshness, ItemState, Readiness};
use crate::resolver::{Config, Resolved};
use crate::runner::{CommandRunner, RealRunner};
use std::path::{Path, PathBuf};

/// A branch's checkout: where it is, its HEAD, and whether it has uncommitted changes.
#[derive(Debug, Clone, Default)]
pub struct Head {
    pub dir: Option<PathBuf>,
    pub sha: Option<String>,
    pub dirty: bool,
}

fn git_in(dir: &Path, args: &[&str]) -> Option<String> {
    let d = dir.to_string_lossy();
    let mut full = vec!["-C", d.as_ref()];
    full.extend_from_slice(args);
    let out = RealRunner.git(&full);
    out.success.then(|| out.stdout.trim().to_string())
}

/// Where `ctx`'s branch is checked out, and its HEAD there.
pub fn head(cfg: &Config, ctx: &Resolved) -> Head {
    let dir = crate::lifecycle::worktree_path(cfg, &ctx.repo, &ctx.branch);
    let sha = dir
        .as_deref()
        .and_then(|d| git_in(d, &["rev-parse", "HEAD"]))
        .filter(|s| !s.is_empty());
    let dirty = dir
        .as_deref()
        .and_then(|d| git_in(d, &["status", "--porcelain"]))
        .is_some_and(|s| !s.is_empty());
    Head { dir, sha, dirty }
}

/// Readiness for `ctx` at its current head, under `[proof].freshness`.
pub fn readiness_now(
    cfg: &Config,
    ctx: &Resolved,
    freshness: Option<Freshness>,
) -> (Readiness, Head) {
    let head = head(cfg, ctx);
    let done_when = crate::store::read(&cfg.note_file(ctx))
        .map(|n| n.front.done_when)
        .unwrap_or_default();
    let listing = list(cfg, ctx);
    let (verdicts, _) = verdicts(cfg, ctx);
    let freshness = freshness
        .or_else(|| Freshness::parse(&cfg.proof_freshness))
        .unwrap_or_default();
    let dir = head.dir.clone();
    let is_ancestor = |sha: &str, tip: &str| {
        dir.as_deref()
            .is_some_and(|d| git_in(d, &["merge-base", "--is-ancestor", sha, tip]).is_some())
    };
    let r = derive(
        &done_when,
        &listing.proofs,
        &verdicts,
        head.sha.as_deref(),
        freshness,
        &is_ancestor,
    );
    (r, head)
}

/// What `bn done` should do before ticking a done-when criterion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoneCheck {
    /// Not a criterion, enforcement off, or accepted evidence exists.
    Clear,
    /// Tick it, but say it is unproven.
    Warn(String),
    /// Don't tick it.
    Refuse(String),
}

/// Check `query` (what was passed to `bn done`) against `[proof].done_requires`. Only a query that
/// names a done-when criterion — its `w<N>` id, or text matching exactly one open criterion and no
/// open todo — is checked; everything else is [`DoneCheck::Clear`].
pub fn check_done(cfg: &Config, ctx: &Resolved, query: &str) -> DoneCheck {
    let policy = cfg.proof_done_requires.as_str();
    if policy == "off" {
        return DoneCheck::Clear;
    }
    let Ok(note) = crate::store::read(&cfg.note_file(ctx)) else {
        return DoneCheck::Clear;
    };
    let q = query.trim();
    let needle = q.to_lowercase();
    let open_w: Vec<_> = note
        .front
        .done_when
        .iter()
        .filter(|w| w.done != Some(true))
        .collect();
    let target = open_w.iter().copied().find(|w| w.id == q).or_else(|| {
        let hits: Vec<_> = open_w
            .iter()
            .copied()
            .filter(|w| w.text.to_lowercase().contains(&needle))
            .collect();
        let todo_hit =
            note.front.todos.iter().any(|t| {
                t.is_open_todo() && (t.id == q || t.text.to_lowercase().contains(&needle))
            });
        (hits.len() == 1 && !todo_hit && !needle.is_empty()).then(|| hits[0])
    });
    let Some(w) = target else {
        return DoneCheck::Clear;
    };
    let (r, _) = readiness_now(cfg, ctx, None);
    let state = r.item(&w.id).map(|i| i.state.clone());
    let why = match state {
        Some(ItemState::Accepted { .. }) | None => return DoneCheck::Clear,
        Some(ItemState::Unproven) => "has no proof".to_string(),
        Some(ItemState::Attached { proofs }) => {
            format!("has proof ({}) nobody has accepted yet", proofs.join(", "))
        }
        Some(ItemState::Stale { proof, .. }) => {
            format!("was accepted on {proof}, but the branch has moved since")
        }
        Some(ItemState::Rejected { proof, .. }) => format!("had its proof ({proof}) rejected"),
    };
    let msg = format!(
        "{} {why}. Evidence first: `bn verify --for {id}` or `bn proof add --kind manual --for {id} \
         --title …`, then someone other than its author runs `bn proof judge <p> accept`.",
        w.id,
        id = w.id
    );
    if policy == "refuse" {
        DoneCheck::Refuse(msg)
    } else {
        DoneCheck::Warn(msg)
    }
}
