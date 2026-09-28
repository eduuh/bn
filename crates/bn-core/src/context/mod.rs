//! Context profiles: the standing context a seat is handed, composed from named **atoms** into a
//! situation-shaped **profile** with a token budget.
//!
//! An atom is one piece — the branch's directives, its goal, done-when readiness, the recap, the
//! handoff, this repo's reference index, one reference section, a skill, a file section, the
//! seat's queue rows. A profile is an ordered list of atoms plus a budget. `<repo>/.bn/context.toml`
//! (with a gitignored `.bn/context.local.toml` overlay) declares custom atoms and profiles; the
//! built-in `ship`, `scout` and `compact-restore` profiles apply when it declares none.
//!
//! Every composition reports what each atom costs and whether the whole overshoots its budget.
//! Serving a profile does not prove an agent read it: delivery and comprehension are separate
//! facts, which is why served packs are logged apart from acknowledgements.

mod atoms;
mod compose;
mod manifest;
pub mod served;

pub use atoms::{render_atom, Atom, AtomInputs};
pub use compose::{compose, Composed, Piece};
pub use manifest::{load, Manifest, Profile, BUILTIN_PROFILES};

/// Where `ctx`'s manifest (and `skill`/`file` atoms) are read from: the branch's own checkout.
pub fn root_for(
    cfg: &crate::resolver::Config,
    ctx: &crate::resolver::Resolved,
) -> Option<std::path::PathBuf> {
    crate::lifecycle::worktree_path(cfg, &ctx.repo, &ctx.branch)
}

#[cfg(test)]
mod tests;
