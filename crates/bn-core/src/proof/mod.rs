//! Proof on disk: evidence for a branch's `done-when` criteria, kept beside its note.
//!
//! A proof (`p<N>`) is a file under `<note_dir>/proof/` with typed frontmatter — what kind of
//! evidence, which criteria (`w<N>`) it speaks to, the commit it was taken at, who produced it —
//! plus an optional attachment (a log, a screenshot). A verdict is a separate, attributed line in
//! `proof/verdicts.jsonl`: someone other than the author accepts or rejects it.
//!
//! The separations are the point:
//! * attaching evidence is not acceptance — a proof with no verdict counts for nothing yet;
//! * the author cannot accept their own proof;
//! * readiness is *derived* from proofs and verdicts, never written by hand, and a file dropped
//!   into `proof/` without frontmatter stays unregistered forever;
//! * a clean readiness says what was accepted, not that the work is good.

mod gate;
mod model;
mod readiness;
mod render;
mod store;
pub mod tee;

pub use gate::{check_done, head, readiness_now, DoneCheck, Head};
pub use model::{Judgement, Kind, NewProof, Proof, Verdict};
pub use readiness::{derive, Freshness, ItemReadiness, ItemState, Readiness};
pub use render::render_proof_md;
pub use store::{add, dir, judge, link, list, verdicts, Listing, ProofError};

/// Who is acting on proof: an explicit `--as`, then `$BN_SEAT`, then — when an agent harness is
/// driving — this worktree's `repo:branch`, else the person at the keyboard (`human:$USER`).
/// Unlike the queue, a person typing in a worktree is not the branch's agent, so the worktree
/// only counts under a harness: otherwise a human could never judge their agent's proof.
pub fn identity(
    flag: Option<&str>,
    env: Option<&str>,
    harness: bool,
    here: Option<&str>,
    user: &str,
) -> crate::coord::Result<String> {
    let here = here.filter(|_| harness);
    crate::coord::acting_seat(flag, env, here, user)
}

/// `p3` for proof 3.
pub fn pid(n: u32) -> String {
    format!("p{n}")
}

/// Parse `p3` or `3`.
pub fn parse_pid(s: &str) -> Option<u32> {
    let s = s.trim();
    s.strip_prefix('p')
        .unwrap_or(s)
        .parse::<u32>()
        .ok()
        .filter(|n| *n > 0)
}

#[cfg(test)]
mod tests;
