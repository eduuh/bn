//! The standing context every freshly-dispatched agent is seeded with: the `ship` (or `scout`)
//! context profile.
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
//! The pieces are atoms of [`bn_core::context`], so a repo can reshape what its agents start with
//! in `.bn/context.toml` (a `ship` or `scout` profile there replaces the built-in one). Each seed
//! is logged as served — which is not the same as read.
//!
//! Kept out of `dispatch.rs`, which is at its file-size ratchet and cannot grow.

use bn_core::resolver::{Config, Resolved};

/// The seed suffix: the `ship` profile (or `scout`) composed for this branch, with `refs` as the
/// dispatch's `--ref` selection. Empty when there is nothing to say, so a caller can append it
/// unconditionally. A profile that can't compose (a broken manifest) falls back to the built-in,
/// because a dispatch must not fail over its seed.
pub(crate) fn section(cfg: &Config, repo: &str, branch: &str, scout: bool, refs: &[String]) -> String {
    let ctx = Resolved {
        repo: repo.to_string(),
        branch: branch.to_string(),
    };
    let profile = if scout { "scout" } else { "ship" };
    let composed = crate::cmd::context::compose(cfg, &ctx, profile, refs, None, false)
        .or_else(|_| builtin(cfg, &ctx, profile, refs));
    match composed {
        Ok(c) => {
            let text = c.text();
            if !text.is_empty() {
                let atoms: Vec<String> = c.pieces.iter().map(|p| p.atom.clone()).collect();
                bn_core::util::best_effort(
                    bn_core::context::served::record(cfg, &ctx, profile, &atoms, &text, "dispatch"),
                    "seed: log served context",
                );
            }
            text
        }
        Err(e) => {
            eprintln!("bn dispatch: seed context not composed ({e})");
            String::new()
        }
    }
}

fn builtin(
    cfg: &Config,
    ctx: &Resolved,
    profile: &str,
    refs: &[String],
) -> Result<bn_core::context::Composed, String> {
    let inputs = bn_core::context::AtomInputs {
        cfg,
        ctx,
        root: None,
        global_refs: refs,
    };
    bn_core::context::compose(&Default::default(), profile, &inputs, None, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seed an agent gets is exactly what it got before profiles existed: standards pointer,
    /// repo note index, then directives, in that order and wording.
    #[test]
    fn the_builtin_ship_profile_is_the_old_seed_byte_for_byte() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = bn_core::test_support::config(temp.path());
        let ctx = Resolved {
            repo: "zz-no-such-repo".into(),
            branch: "feat".into(),
        };
        std::fs::create_dir_all(cfg.note_dir(&ctx)).unwrap();
        bn_core::commands::add(&cfg, &ctx, "directive", "never touch vendor/", None, "2026-09-25")
            .unwrap();
        let refs = vec!["-".to_string()];
        let text = builtin(&cfg, &ctx, "ship", &refs).unwrap().text();
        assert_eq!(
            text,
            "\n\nSTANDING INSTRUCTIONS for this branch — follow these for everything you do here. \
             They were recorded with `bn agent instruct` and live in this branch's note under \
             `## Directives`, so they apply to every session on this branch, not just this \
             one:\n- never touch vendor/\n"
        );
        assert_eq!(builtin(&cfg, &ctx, "scout", &refs).unwrap().text(), text);
    }
}
