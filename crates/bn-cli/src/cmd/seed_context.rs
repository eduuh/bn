//! The standing context every freshly-dispatched agent is seeded with.
//!
//! Three kinds, all "things that outlive any one session":
//!
//!   * **references** (`bn ref`, #623) — person-scoped coding standards that apply on every repo,
//!   * **reference notes** (`bn ref --repo`) — what past sessions discovered about *this* repo,
//!   * **directives** (`bn agent instruct`) — branch-scoped standing instructions you gave *this*
//!     lane by hand.
//!
//! Directives are the reason this module exists. Before it, an instruction was a message in a pane:
//! relaunch the agent, or dispatch a fresh one onto the same branch, and it was gone — so the same
//! guidance had to be re-typed after every reset. Folding the note's `## Directives` into the seed
//! (as [`crate::cmd::agent`]'s handoff half does for `--hard` relaunch) makes "I already told it
//! that" true across restarts.
//!
//! Kept out of `dispatch.rs`, which is at its file-size ratchet and cannot grow.

use bn_core::resolver::{Config, Resolved};

/// The seed suffix carrying both kinds of standing context. Empty when there is nothing to say, so
/// a caller can append it unconditionally.
///
/// A scout may graduate into code once its investigation narrows the change, so it gets the same
/// coding-standards references as a ship. Directives apply either way: "don't touch the vendored
/// directory" constrains an investigation exactly as much as a ship.
pub(crate) fn section(
    cfg: &Config,
    repo: &str,
    branch: &str,
    _scout: bool,
    refs: &[String],
) -> String {
    let mut out = String::new();
    if let Some(s) = super::dispatch::references_section(refs) {
        out.push_str(&s);
    }
    // Repo notes go to scouts too: a scout changes no code, so the coding standards don't apply,
    // but "what we already know about this repo" is exactly what an investigation should start from.
    if let Some(s) = repo_references_section(repo) {
        out.push_str(&s);
    }
    if let Some(s) = directives_section(cfg, repo, branch) {
        out.push_str(&s);
    }
    out
}

/// The repo's reference notes, **by name only**, with the command to read one. `None` when the
/// repo has no notes yet.
///
/// Names only is the whole point: an index of a few dozen notes costs a couple of hundred tokens,
/// so every agent can afford to know what exists, and pays for a body only when it decides it
/// needs that body. Inlining the notes themselves would blow the seed on the first repo that
/// accumulates real knowledge.
pub(crate) fn repo_references_section(repo: &str) -> Option<String> {
    let names = bn_core::references::names_in(&bn_core::references::Scope::repo(repo));
    if names.is_empty() {
        return None;
    }
    Some(format!(
        "\n\nThis repo's reference notes — what past sessions discovered about it (how it builds, \
         where the seams are, which failures mislead). Read the one that bears on your task with \
         `bn ref <name> --repo`, and when you learn something durable, record it with \
         `bn ref append <name> --repo --section gotchas \"<fact> (src: …, <date>)\"`: {}.",
        names.join(", ")
    ))
}

/// The branch note's standing directives, rendered for a seed. `None` when there are none.
pub(crate) fn directives_section(cfg: &Config, repo: &str, branch: &str) -> Option<String> {
    let ctx = Resolved { repo: repo.to_string(), branch: branch.to_string() };
    let note = bn_core::store::read(&cfg.note_file(&ctx)).ok()?;
    let items: Vec<String> = note.front.directives.iter().map(|d| d.text.clone()).collect();
    if items.is_empty() {
        return None;
    }
    let list = items
        .iter()
        .map(|d| format!("\n- {d}"))
        .collect::<Vec<_>>()
        .join("");
    Some(format!(
        "\n\nSTANDING INSTRUCTIONS for this branch — follow these for everything you do here. They \
         were recorded with `bn agent instruct` and live in this branch's note under \
         `## Directives`, so they apply to every session on this branch, not just this one:{list}\n"
    ))
}
