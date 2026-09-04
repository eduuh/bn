//! The landing recipe: what a worktree's editor should be *showing* when you arrive.
//!
//! Opening a worktree should not drop you into an empty editor. It should land you on this
//! branch's diff, with the agent terminal already spawned — so the first thing on screen is what
//! changed and what the agent is doing.
//!
//! bn owns the **data**, nvim owns the **verbs**. The spec below is a declarative table handed to
//! one named entrypoint, `require('bn').land(spec)`, rather than a list of `:` commands. A command
//! list would put Diffview and toggleterm API details inside bn — a repo bn does not ship and
//! cannot test — and, worse, would make idempotency unexpressible: bn would have to interrogate
//! nvim about existing tabs and terminals before deciding what to send, which is three round trips
//! and a race. A named entrypoint is one versioned contract, it can answer, and the plugin stays
//! free to change *how* it satisfies it.
//!
//! The spec is written to a file and referenced by path, never interpolated into a command line:
//! it survives tmux's parsing and the shell with a single quote, hits no argv length limit, and
//! the warm (`--remote-expr`) path re-reads the very bytes the cold (`nvim -c`) path was given.

use super::rpc;
use super::session::EditSession;
use super::target::EditTarget;
use bn_core::resolver::{Config, Resolved};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// The version of the spec shape. The Lua side refuses anything it does not recognize rather than
/// half-applying a landing it does not understand.
const SPEC_VERSION: u32 = 1;

/// What the agent terminal should be doing when you land.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum TermMode {
    /// Spawned and warm, but not shown — the headline of a landing is the diff, and a terminal on
    /// top of it defeats the point. The default when *you* opened the worktree.
    #[default]
    Hidden,
    /// Spawned and on screen. The right default when an *agent* is driving and the first question
    /// is what it is doing.
    Visible,
    /// Do not spawn one at all.
    Off,
}

impl TermMode {
    pub(crate) fn parse(s: &str) -> TermMode {
        match s.trim() {
            "visible" => TermMode::Visible,
            "off" => TermMode::Off,
            _ => TermMode::Hidden,
        }
    }
}

/// The contract handed to `require('bn').land`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Recipe {
    pub(crate) v: u32,
    pub(crate) worktree: String,
    pub(crate) repo: String,
    pub(crate) branch: String,
    /// Already resolved by bn — nvim never guesses a base. This is what makes the cold and warm
    /// paths identical, and what keeps a diff view agreeing with `bn diff`'s preview.
    pub(crate) base: String,
    pub(crate) diff: bool,
    pub(crate) term: TermMode,
    /// The command the agent terminal runs; `null` for a plain shell.
    pub(crate) agent_cmd: Option<String>,
    pub(crate) files: Vec<String>,
    pub(crate) note: Option<String>,
    /// Re-apply even when the fingerprint says this landing already happened.
    pub(crate) force: bool,
}

/// What the caller asked for.
#[derive(Debug, Clone, Default)]
pub(crate) struct LandOpts {
    pub(crate) base: Option<String>,
    pub(crate) diff: bool,
    pub(crate) term: TermMode,
    pub(crate) agent_cmd: Option<String>,
    pub(crate) force: bool,
}

/// What nvim reported back.
#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct LandStatus {
    #[serde(default)]
    pub(crate) ok: bool,
    /// False when the fingerprint matched and nothing needed doing — a successful no-op, not a
    /// failure.
    #[serde(default)]
    pub(crate) landed: bool,
    #[serde(default)]
    pub(crate) did: Vec<String>,
    #[serde(default)]
    pub(crate) why: Option<String>,
}

/// Resolve the base ref for a landing.
///
/// Precedence: what the caller said, then the note's recorded PR base if there is one, then the
/// repo's own answer via [`bn_core::gitbase`], then `origin/main` as a last resort. Resolved here,
/// in bn, *before* nvim starts.
pub(crate) fn resolve_base(target: &EditTarget, asked: Option<&str>) -> String {
    if let Some(b) = asked.filter(|b| !b.trim().is_empty()) {
        return b.trim().to_string();
    }
    bn_core::gitbase::base_ref(
        &bn_core::runner::RealRunner,
        bn_core::gitbase::BaseScope::Worktree(&target.worktree),
    )
    .unwrap_or_else(|| "origin/main".to_string())
}

/// Build the recipe for a target.
pub(crate) fn recipe_for(cfg: &Config, target: &EditTarget, opts: &LandOpts) -> Recipe {
    let ctx = Resolved {
        repo: target.repo.clone(),
        branch: target.branch.clone(),
    };
    let note = cfg.note_file(&ctx);
    Recipe {
        v: SPEC_VERSION,
        worktree: target.worktree.display().to_string(),
        repo: target.repo.clone(),
        branch: target.branch.clone(),
        base: resolve_base(target, opts.base.as_deref()),
        diff: opts.diff,
        term: opts.term,
        agent_cmd: opts.agent_cmd.clone(),
        files: Vec::new(),
        note: note.exists().then(|| note.display().to_string()),
        force: opts.force,
    }
}

/// Write the spec beside the socket and return its path.
pub(crate) fn write_spec(target: &EditTarget, recipe: &Recipe) -> Result<PathBuf, String> {
    bn_core::editor::ensure_runtime_dir();
    let path = bn_core::editor::spec_file_for(&target.worktree);
    let body = serde_json::to_string_pretty(recipe).map_err(|e| e.to_string())?;
    std::fs::write(&path, body).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Apply a recipe to an editor that is already running.
///
/// The cold path does not come through here: a freshly created pane is given the same spec path on
/// its own command line, so it lands as part of starting up rather than being told to afterwards.
pub(crate) fn apply(sess: &EditSession, spec: &Path) -> Result<LandStatus, String> {
    let out = rpc::remote_expr(&sess.socket, &rpc::land_expr(spec), rpc::LAND_TIMEOUT)
        .map_err(|e| e.to_string())?;
    parse_status(&out)
}

/// Parse what the Lua entrypoint returned.
///
/// An editor without the plugin answers with an error rather than JSON; say so plainly instead of
/// reporting a landing that never happened.
pub(crate) fn parse_status(out: &str) -> Result<LandStatus, String> {
    let trimmed = out.trim();
    if trimmed.is_empty() {
        return Err("editor returned nothing — is the bn Lua module on its runtimepath?".into());
    }
    serde_json::from_str::<LandStatus>(trimmed).map_err(|_| {
        format!("editor did not return a landing status (got: {trimmed}) — is the bn Lua module installed?")
    })
}

#[cfg(test)]
mod status_tests {
    use super::*;

    #[test]
    fn a_landing_that_did_work_is_reported_as_landed() {
        let s = parse_status(r#"{"ok":true,"landed":true,"did":["diff","term"]}"#).unwrap();
        assert!(s.ok && s.landed);
        assert_eq!(s.did, vec!["diff", "term"]);
    }

    #[test]
    fn an_already_landed_editor_is_a_success_not_a_failure() {
        // Idempotency: re-landing must not stack a second Diffview tab, and reporting that as an
        // error would make a harmless repeat look broken.
        let s = parse_status(r#"{"ok":true,"landed":false,"why":"already"}"#).unwrap();
        assert!(s.ok);
        assert!(!s.landed);
        assert_eq!(s.why.as_deref(), Some("already"));
    }

    #[test]
    fn a_missing_plugin_is_named_rather_than_silently_passing() {
        // `--remote-expr` against an nvim without the module prints a VimL error, not JSON.
        let e = parse_status("E5108: Error executing lua ...module 'bn' not found").unwrap_err();
        assert!(e.contains("bn Lua module"));
    }

    #[test]
    fn empty_output_is_an_error_not_an_empty_landing() {
        assert!(parse_status("   ").is_err());
    }
}

#[cfg(test)]
mod term_tests {
    use super::*;

    #[test]
    fn term_modes_round_trip_through_their_wire_names() {
        assert_eq!(TermMode::parse("visible"), TermMode::Visible);
        assert_eq!(TermMode::parse("off"), TermMode::Off);
        assert_eq!(TermMode::parse("hidden"), TermMode::Hidden);
        // Anything unrecognized lands hidden: a spawned-but-unseen terminal is the safe default,
        // never "silently do not spawn one".
        assert_eq!(TermMode::parse("nonsense"), TermMode::Hidden);
        assert_eq!(
            serde_json::to_string(&TermMode::Visible).unwrap(),
            "\"visible\""
        );
    }
}
