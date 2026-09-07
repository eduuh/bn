//! The repo's own job scripts as picker verbs — the rows below the roster.
//!
//! [`bn_core::wt_actions`] is the roster of verbs every repo has. A repo also has verbs of its
//! own: the `*.sh` under `<notes>/jobs/<repo>/` (synced through the notes repo) and `.bn/jobs/`
//! (committed with the project), which `bn build <name>` and the cron already resolve through
//! [`bn_core::resolver::job_dirs`]. Until now the picker could not see them, so running BigMono's
//! `start.sh` meant leaving the list and remembering the command.
//!
//! A script row queues like a roster row — the same `ExecSpec::Job`, the same entry id derivation,
//! the same ctrl-o menu — with two things pinned that the roster gets from `job_registry.rs`:
//!
//! * **The branch is `main`.** The dispatcher runs `bn run <name> --repo R --branch B`, and
//!   `--repo` without `--branch` is refused outright; a branch is also what lets `job_dirs` reach
//!   the Repo and Project tiers at all. A native verb never noticed because it ignores its ctx.
//! * **It runs in a tmux window at the repo's main checkout.** That is the heavy path, which
//!   `enqueue_action` now takes for every picker verb; a script is the case that made it
//!   mandatory, because it inherits the window's cwd and most of them `git rev-parse` from there.
//!
//! The verb itself is a [`WtAction`] built at runtime and leaked. Every picker callback — the side
//! pane, Enter, `ctrl-o`, `ctrl-s` — is its own short-lived `bn` process, so the leak is one
//! struct and two strings freed at exit, and in exchange nothing that takes `&WtAction` changes and
//! the roster in bn-core stays the pure static table its doc promises.
use bn_core::resolver::{Config, JobScope};
use bn_core::wt_actions::{Scope, WtAction};
use std::path::PathBuf;

/// One of the repo's own scripts, as found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::tat) struct RepoScript {
    /// The job name — the file name without `.sh`, and what `bn run` resolves.
    pub(in crate::tat) name: String,
    /// Which tier it came from. Shown on the row, since a committed `.bn/jobs` script and a
    /// notes-synced one are maintained in different places.
    pub(in crate::tat) scope: JobScope,
    pub(in crate::tat) path: PathBuf,
}

/// The payload prefix that tells a `cmd` row's id from a roster id.
const PREFIX: &str = "script:";

/// The row id for a script name.
pub(in crate::tat) fn id(name: &str) -> String {
    format!("{PREFIX}{name}")
}

/// The script name inside a row id, or `None` for a roster id or a malformed one.
pub(in crate::tat) fn name_of(id: &str) -> Option<&str> {
    id.strip_prefix(PREFIX).filter(|name| is_valid_name(name))
}

/// A name a row can carry. It rides fzf's `|`-delimited row and the `~`-joined `--pick-arg`, and
/// becomes a tmux window name and half of a queue subject, so anything that could split a field or
/// escape a directory is refused here rather than discovered later.
fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '|' | '~' | '/' | '\\' | ':'))
}

/// Whether a script found on disk is worth a row.
///
/// Not offered: `build`, which "Build latest main" already runs (with a sync first) and which the
/// pool warms with; any name a roster verb already answers to; and any name the native job
/// registry claims — `run_job_inner` runs the builtin *before* looking for a script, so a
/// `verify.sh` would render as a script and run the native verify.
fn offerable(name: &str) -> bool {
    is_valid_name(name)
        && name != "build"
        && !bn_core::wt_actions::all()
            .iter()
            .any(|a| a.id == name || a.job == name)
        && crate::cmd::job_registry::native_job(name).is_none()
}

/// The scripts the picker offers for `repo`, in tier order then by name.
///
/// The context is resolved the way `bn run --repo R --branch main` will resolve it at execution
/// (`resolve_target` lowercases the repo), so the directories searched here are the directories
/// the dispatcher will search then. Only the Repo and Project tiers are read: the three global
/// tiers hold machine jobs, not this repo's verbs, and this runs on every 700ms repaint.
pub(in crate::tat) fn repo_scripts(cfg: &Config, repo: &str) -> Vec<RepoScript> {
    let Some(ctx) = main_ctx(cfg, repo) else {
        return Vec::new();
    };
    scripts_in(&bn_core::resolver::job_dirs(cfg, Some(&ctx)))
}

/// The `(repo, main)` context a script is queued against — one function so the preview, the
/// existence check and the enqueue all resolve the same way.
fn main_ctx(cfg: &Config, repo: &str) -> Option<bn_core::resolver::Resolved> {
    let cwd = std::env::current_dir().unwrap_or_default();
    bn_core::resolver::resolve_target(cfg, &cwd, Some(repo.to_string()), Some("main".to_string()))
        .ok()
}

/// The pure half of [`repo_scripts`]: which of these tiers' files become rows. First tier wins a
/// name, matching `job_script`'s resolution order, so the row and the run agree on which file.
fn scripts_in(dirs: &[(JobScope, PathBuf)]) -> Vec<RepoScript> {
    let mut out: Vec<RepoScript> = Vec::new();
    for (scope, dir) in dirs
        .iter()
        .filter(|(scope, _)| matches!(scope, JobScope::Repo | JobScope::Project))
    {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                e.file_name()
                    .to_string_lossy()
                    .strip_suffix(".sh")
                    .map(str::to_string)
            })
            .filter(|name| offerable(name))
            .collect();
        names.sort();
        for name in names {
            if out.iter().any(|s| s.name == name) {
                continue;
            }
            out.push(RepoScript {
                path: dir.join(format!("{name}.sh")),
                name,
                scope: *scope,
            });
        }
    }
    out
}

/// The verb for a script name, with the roster's shape and a `'static` lifetime.
///
/// `needs_branch` is true so `enqueue_action` stamps `branch: main` — see the module doc for why
/// that is mandatory rather than a convenience. The label is the bare name: it carries no
/// whitespace, and every roster label does, so `action_subject` (and the entry id derived from it)
/// cannot collide with a roster verb's.
pub(in crate::tat) fn verb(name: &str) -> &'static WtAction {
    let name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let id: &'static str = Box::leak(id(name).into_boxed_str());
    Box::leak(Box::new(WtAction {
        id,
        label: name,
        scope: Scope::Repo,
        job: name,
        needs_branch: true,
    }))
}

/// A row payload to its verb: a roster id, or a well-formed script id. Syntactic only — whether
/// the script still exists is [`on_disk`]'s question, asked where a refusal can be shown.
pub(in crate::tat) fn resolve(id: &str) -> Option<&'static WtAction> {
    bn_core::wt_actions::find(id).or_else(|| name_of(id).map(verb))
}

/// Where `name` resolves for `repo` right now, or `None` when no tier has it any more — a row can
/// outlive the file it was painted from.
pub(in crate::tat) fn on_disk(cfg: &Config, repo: &str, name: &str) -> Option<(JobScope, PathBuf)> {
    let ctx = main_ctx(cfg, repo)?;
    bn_core::resolver::job_script(cfg, Some(&ctx), name)
}

/// The side pane for a script row: where it lives, what it declared, where it will run.
pub(in crate::tat) fn preview(cfg: &Config, repo: &str, name: &str) {
    println!("\x1b[1;36m{name}\x1b[0m\n");
    let Some((scope, path)) = on_disk(cfg, repo, name) else {
        println!("\x1b[1;31mUnavailable:\x1b[0m no `{name}.sh` for {repo} any more.");
        println!("The row was painted before the file went away; ctrl-r reloads the list.");
        return;
    };
    let header = bn_core::resolver::script_header(&path);
    for line in &header.comments {
        println!("{line}");
    }
    if !header.comments.is_empty() {
        println!();
    }
    println!("Repo:   {repo}");
    println!("Script: {} ({} tier)", path.display(), scope.label());
    println!("Runs:   in a tmux window named `{name}` in the {repo} session,");
    println!("        at {}", run_dir(cfg, repo).display());
    if header.latest_main {
        println!("        after fetching and fast-forwarding main (# bn: latest-main)");
    }
    println!("        heavy — holds a heavy slot while it runs; the window closes when it exits 0");
    super::queue_state_lines(cfg, repo, verb(name));
}

/// The directory the heavy window opens in — the same answer the dispatcher gives itself.
fn run_dir(cfg: &Config, repo: &str) -> PathBuf {
    let (_, worktree_dir) = crate::wt::layout(cfg);
    crate::wt::respawn_fallback(&worktree_dir, repo)
}

/// The tier word a row shows.
pub(in crate::tat) fn tier(scope: JobScope) -> &'static str {
    match scope {
        JobScope::Project => "project script",
        _ => "repo script",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn tier_dir(tmp: &Path, scope: JobScope, names: &[&str]) -> (JobScope, PathBuf) {
        let dir = tmp.join(scope.label());
        std::fs::create_dir_all(&dir).unwrap();
        for n in names {
            std::fs::write(dir.join(format!("{n}.sh")), "#!/bin/sh\n").unwrap();
        }
        (scope, dir)
    }

    fn names(scripts: &[RepoScript]) -> Vec<&str> {
        scripts.iter().map(|s| s.name.as_str()).collect()
    }

    /// BigMono's own jobs dir, as shipped: `build` is the roster's business, the other two are rows.
    #[test]
    fn build_is_left_to_the_roster_and_the_rest_are_offered() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = vec![tier_dir(
            tmp.path(),
            JobScope::Repo,
            &["start", "build", "build-web-app"],
        )];
        assert_eq!(
            names(&scripts_in(&dirs)),
            vec!["build-web-app", "start"]
        );
    }

    /// The runner looks for a native job before a script, so a script that shadows one would
    /// render as itself and run as the builtin. Same for a roster name.
    #[test]
    fn a_script_named_like_a_native_or_roster_job_is_not_offered() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = vec![tier_dir(
            tmp.path(),
            JobScope::Repo,
            &["verify", "pool-warm", "wt-cleanup", "fleet-reap", "deploy"],
        )];
        assert_eq!(names(&scripts_in(&dirs)), vec!["deploy"]);
    }

    /// The Project tier is listed first by `job_dirs` and wins on run, so it wins the row too;
    /// the Repo tier's copy of the same name is not a second row.
    #[test]
    fn the_higher_tier_shadows_the_lower_one() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = vec![
            tier_dir(tmp.path(), JobScope::Project, &["start"]),
            tier_dir(tmp.path(), JobScope::Repo, &["start", "deploy"]),
        ];
        let scripts = scripts_in(&dirs);
        assert_eq!(names(&scripts), vec!["start", "deploy"]);
        assert_eq!(scripts[0].scope, JobScope::Project);
        assert!(scripts[0].path.ends_with("project/start.sh"));
    }

    /// Global tiers hold machine jobs — `commit-notes`, `ado-reviews` — not this repo's verbs.
    #[test]
    fn global_and_branch_tiers_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = vec![
            tier_dir(tmp.path(), JobScope::Branch, &["branchy"]),
            tier_dir(tmp.path(), JobScope::Global, &["commit-notes"]),
        ];
        assert!(scripts_in(&dirs).is_empty());
    }

    /// A name rides fzf's row and `--pick-arg`, names a tmux window, and is half a queue subject.
    #[test]
    fn names_that_could_split_a_field_or_escape_a_dir_are_refused() {
        for bad in ["", "a|b", "a~b", "a/b", "a b", ".hidden", "a:b", "a\\b"] {
            assert!(!is_valid_name(bad), "{bad:?}");
        }
        for good in ["start", "build-web-app", "run_tests", "e2e.smoke"] {
            assert!(is_valid_name(good), "{good:?}");
        }
    }

    #[test]
    fn ids_round_trip_and_reject_malformed_payloads() {
        assert_eq!(name_of(&id("start")), Some("start"));
        assert_eq!(name_of("script:a|b"), None);
        assert_eq!(name_of("script:"), None);
        assert_eq!(name_of("pool-warm"), None);
    }

    /// A roster id resolves to the roster's own entry; a script id to a Repo-scope verb whose job
    /// is the script name and which pins a branch.
    #[test]
    fn resolve_answers_for_both_kinds_of_id() {
        let roster = resolve("pool-warm").expect("roster id");
        assert_eq!(roster.job, "pool-warm");
        let script = resolve("script:start").expect("script id");
        assert_eq!(script.id, "script:start");
        assert_eq!(script.job, "start");
        assert_eq!(script.label, "start");
        assert_eq!(script.scope, Scope::Repo);
        assert!(
            script.needs_branch,
            "the branch is what makes `bn run --repo` accept it"
        );
        assert_eq!(resolve("script:a|b"), None);
        assert_eq!(resolve("no-such-verb"), None);
    }
}
