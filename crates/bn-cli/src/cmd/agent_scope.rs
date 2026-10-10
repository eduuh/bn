//! Where a launched agent may look — the harness argv that fixes its allowed working set, and the
//! directories bn widens that set with.
//!
//! Split from `agent.rs` under the file-size ratchet, but the seam is a real one: everything here
//! is a pure function from a [`LaunchPlan`] to argv, with no process, tmux or lease work, which is
//! what makes the scoping rules testable without launching anything.
//!
//! The rule these encode: an agent is confined to the directory it was started in.
//! [`bn_core::mcp::Harness::autonomy_args`] documents the measurements behind that; this module
//! owns the other half — the directories bn hands back, because bn's own state lives outside the
//! worktree and an agent that cannot reach its own branch note is not scoped, it is broken.

use super::agent::LaunchPlan;
use bn_core::resolver::Config;
use std::path::Path;

/// `--add-dir <dir>` per directory, for either CLI (both spell the flag the same way).
pub(crate) fn add_dir_args(harness: bn_core::mcp::Harness, dirs: &[String]) -> Vec<String> {
    dirs.iter()
        .flat_map(|dir| [harness.add_dir_flag().to_string(), dir.clone()])
        .collect()
}

/// The directories a launch in `cwd` may reach beyond `cwd` itself: bn's own branch-note directory,
/// plus whatever the caller asked for with `--add-dir`.
///
/// The note directory is not a convenience. `<notes>/<machine>/<repo>/<branch>` holds the branch
/// note and the plan artifacts under `plans/`, and the plan/execute skills read and write those
/// files directly rather than through the CLI — so scoping an agent to its worktree without it
/// would break the workflows bn is built around. It is also narrow: one branch's directory, not
/// the notes repository, and nothing resembling `~/Documents`.
///
/// Deduped and order-stable, so a repeated `--add-dir` (or one naming the note dir itself)
/// produces a single flag pair rather than several.
pub(crate) fn launch_add_dirs(cfg: &Config, cwd: &Path, extra: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |dir: String| {
        if !dir.is_empty() && !out.contains(&dir) {
            out.push(dir);
        }
    };
    if let Ok(ctx) = bn_core::resolver::resolve(cfg, cwd) {
        let note_dir = cfg.note_dir(&ctx);
        // Only offer a directory that exists: `--add-dir` on a missing path is an error, and a
        // branch whose note was never created has nothing there to reach anyway.
        if note_dir.is_dir() {
            push(note_dir.to_string_lossy().to_string());
        }
    }
    for dir in extra {
        push(dir);
    }
    out
}

/// Build the argv (after the harness binary) for a **Claude** launch. Resumes with `--continue`
/// when this worktree has transcripts (unless `--fresh`); otherwise starts fresh, pinning the
/// session id and passing the seed as the initial prompt. Pure — mirrors the zsh `claude` branch.
pub(crate) fn build_claude_args(plan: &LaunchPlan, has_transcripts: bool) -> Vec<String> {
    let mut args = Vec::new();
    if plan.yolo() {
        args.extend(
            bn_core::mcp::Harness::Claude
                .autonomy_args()
                .iter()
                .map(|s| s.to_string()),
        );
    }
    args.extend(plan.add_dir_args(bn_core::mcp::Harness::Claude));
    if !plan.fresh {
        if let Some(id) = &plan.resume_session {
            args.extend(["--resume".to_string(), id.clone()]);
            args.extend(plan.passthrough.iter().cloned());
            return args;
        }
    }
    if !plan.fresh && has_transcripts {
        args.push("--continue".to_string()); // resume: session already holds context
        args.extend(plan.passthrough.iter().cloned());
        return args;
    }
    args.extend(plan.session_flag());
    if let Some(seed) = &plan.seed {
        args.push(seed.clone()); // fresh: the seed is Claude's initial prompt (positional)
    }
    args.extend(plan.passthrough.iter().cloned());
    args
}

/// Build the argv (after the harness binary) for a **Copilot** launch. Resumes the resolved session
/// id with `--resume=` when one exists (unless `--fresh`); otherwise starts fresh, pinning the
/// session id and seeding the first turn with `-i <seed>`. Pure — mirrors the zsh `copilot` branch.
/// Copilot's standing shell approval for bn itself, so an agent can follow the AGENTS.md rule to
/// reap stale worktrees (`bn wt reap --all --yes`) without a prompt. Copilot matches shell rules on
/// the command name only — `shell(bn wt reap:*)` and `shell(bn wt:*)` never match — so the rule
/// has to cover every `bn` subcommand. Claude's narrower `Bash(bn wt reap:*)` lives in its stowed
/// settings instead.
const COPILOT_BN_SHELL_RULE: &str = "shell(bn)";

pub(crate) fn build_copilot_args(plan: &LaunchPlan, resume_id: Option<&str>) -> Vec<String> {
    let mut args = vec!["--allow-tool".to_string(), COPILOT_BN_SHELL_RULE.to_string()];
    if plan.yolo() {
        args.extend(
            bn_core::mcp::Harness::Copilot
                .autonomy_args()
                .iter()
                .map(|s| s.to_string()),
        );
    }
    args.extend(plan.add_dir_args(bn_core::mcp::Harness::Copilot));
    if !plan.fresh {
        if let Some(rid) = resume_id {
            args.push(format!("--resume={rid}"));
            args.extend(plan.passthrough.iter().cloned());
            return args;
        }
    }
    args.extend(plan.session_flag());
    if let Some(seed) = &plan.seed {
        args.push("-i".to_string()); // copilot "start interactive and run this prompt"
        args.push(seed.clone());
    }
    args.extend(plan.passthrough.iter().cloned());
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use bn_core::mcp::Harness;

    fn plan(seed: Option<&str>, yolo: bool) -> LaunchPlan {
        LaunchPlan {
            seed: seed.map(str::to_string),
            yolo_force: yolo,
            fresh: true,
            session: None,
            resume_session: None,
            add_dirs: Vec::new(),
            passthrough: Vec::new(),
        }
    }

    /// The regression that motivated all of this: an unseeded `claude` through bn's shell wrapper
    /// took `--dangerously-skip-permissions`, which is a *path* bypass as well as a tool one, and
    /// the agent could read `~/Documents`.
    #[test]
    fn an_autonomous_claude_launch_never_bypasses_path_checks() {
        let args = build_claude_args(&plan(None, false), false);
        assert!(
            !args.iter().any(|a| a == "--dangerously-skip-permissions"),
            "{args:?}"
        );
        assert!(args.windows(2).any(|w| w == ["--permission-mode", "acceptEdits"]), "{args:?}");
    }

    /// `--allow-all-paths` is documented by Copilot as "disable file path verification" — the
    /// same hole, spelled differently. `--allow-all` is the yolo launch flag bn now uses once the
    /// slot has already been pre-trusted.
    #[test]
    fn an_autonomous_copilot_launch_uses_allow_all_but_not_paths() {
        let args = build_copilot_args(&plan(None, false), None);
        assert!(!args.iter().any(|a| a == "--allow-all-paths"), "{args:?}");
        assert!(args.iter().any(|a| a == "--allow-all"), "{args:?}");
    }

    #[test]
    fn a_seeded_launch_without_yolo_asks_for_nothing_extra() {
        let args = build_claude_args(&plan(Some("body"), false), false);
        assert!(!args.iter().any(|a| a == "--permission-mode"), "{args:?}");
    }

    /// The allowed working set belongs to the session, not to how its prompts are answered — so a
    /// *resumed* agent must be handed the same directories a fresh one gets. These two paths
    /// return early, which is exactly where an `--add-dir` is easy to drop.
    #[test]
    fn add_dir_survives_every_resume_path() {
        let mut p = plan(None, false);
        p.add_dirs = vec!["/notes/bn/main".into()];
        p.fresh = false;
        p.resume_session = Some("abc".into());
        let resumed = build_claude_args(&p, false);
        assert!(
            resumed.windows(2).any(|w| w == ["--add-dir", "/notes/bn/main"]),
            "claude --resume lost the dir: {resumed:?}"
        );

        let continued = build_claude_args(&plan_continue(), true);
        assert!(
            continued.windows(2).any(|w| w == ["--add-dir", "/notes/bn/main"]),
            "claude --continue lost the dir: {continued:?}"
        );

        let copilot = build_copilot_args(&p, Some("abc"));
        assert!(
            copilot.windows(2).any(|w| w == ["--add-dir", "/notes/bn/main"]),
            "copilot --resume lost the dir: {copilot:?}"
        );
    }

    fn plan_continue() -> LaunchPlan {
        let mut p = plan(None, false);
        p.fresh = false;
        p.add_dirs = vec!["/notes/bn/main".into()];
        p
    }

    /// The stale-worktree rule asks agents to run `bn wt reap` unprompted, so every Copilot launch —
    /// fresh, resumed, or without autonomy — carries the bn shell approval.
    #[test]
    fn every_copilot_launch_may_run_bn() {
        let mut resumed = plan(None, false);
        resumed.fresh = false;
        for args in [
            build_copilot_args(&plan(None, false), None),
            build_copilot_args(&plan(Some("go"), true), None),
            build_copilot_args(&resumed, Some("abc")),
        ] {
            assert!(
                args.windows(2).any(|w| w == ["--allow-tool", "shell(bn)"]),
                "{args:?}"
            );
        }
    }

    #[test]
    fn repeated_directories_produce_one_flag_pair() {
        let args = add_dir_args(Harness::Claude, &["/a".to_string(), "/b".to_string()]);
        assert_eq!(args, vec!["--add-dir", "/a", "--add-dir", "/b"]);
    }
}
