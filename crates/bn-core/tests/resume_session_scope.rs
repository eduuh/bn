//! `bn resume`'s two resolution rules: the session must belong to the branch the checkout is on
//! now, and its transcript must still exist.
//!
//! Both are pool-slot regressions. `…/_pool/2` hosts a different branch every few hours, so a
//! worktree path alone identified the slot rather than the work — a bare `bn resume` there restored
//! whichever branch held the slot last, against code no longer checked out. And a session id whose
//! transcript is gone makes the harness start a blank agent while the command reports a resume.

use bn_core::agent::{
    resolve_session, ResolveSessionError, SessionRecord, SessionSelector, SessionStatus,
};

const SLOT: &str = "/tmp/bn-test/worktree/bn/_pool/2";

fn row(branch: &str, session_id: &str, updated_at: i64) -> SessionRecord {
    SessionRecord {
        task_name: branch.to_string(),
        harness: "claude".into(),
        repo: "bn".into(),
        branch: branch.into(),
        worktree: SLOT.into(),
        session_id: session_id.into(),
        controller_id: None,
        status: SessionStatus::Idle,
        pr_url: None,
        created_at: updated_at,
        updated_at,
    }
}

fn latest<'a>(
    rows: &'a [SessionRecord],
    branch: &str,
) -> Result<&'a SessionRecord, ResolveSessionError> {
    resolve_session(
        rows,
        SessionSelector::LatestForWorktree {
            worktree: SLOT,
            branch,
        },
    )
}

#[test]
fn a_recycled_pool_slot_never_resumes_the_previous_tenant() {
    // The stale row is the *newer* one, so recency alone would pick it.
    let rows = vec![row("mine", "aaa", 100), row("someone-elses", "bbb", 900)];
    assert_eq!(latest(&rows, "mine").unwrap().session_id, "aaa");
}

#[test]
fn branch_spellings_that_differ_only_in_case_and_slashes_still_match() {
    let rows = vec![row("feature/Login-Fix", "aaa", 100)];
    assert_eq!(
        latest(&rows, "feature-login-fix").unwrap().session_id,
        "aaa"
    );
}

#[test]
fn the_newest_session_for_this_branch_wins() {
    let rows = vec![row("mine", "old", 100), row("mine", "new", 200)];
    assert_eq!(latest(&rows, "mine").unwrap().session_id, "new");
}

#[test]
fn a_slot_holding_only_other_branches_reports_nothing_to_resume() {
    let rows = vec![row("someone-elses", "bbb", 900)];
    assert!(matches!(
        latest(&rows, "mine"),
        Err(ResolveSessionError::NoWorktreeSession(_))
    ));
}

#[test]
fn a_detached_checkout_reporting_no_branch_matches_nothing() {
    let rows = vec![row("mine", "aaa", 100)];
    assert!(latest(&rows, "").is_err());
}

#[test]
fn naming_a_session_explicitly_still_ignores_the_branch() {
    let rows = vec![row("someone-elses", "bbb", 900)];
    assert_eq!(
        resolve_session(&rows, SessionSelector::NameOrId("bbb"))
            .unwrap()
            .session_id,
        "bbb"
    );
}

// --- transcript evidence ---------------------------------------------------

#[test]
fn transcripts_decide_whether_an_id_is_resumable() {
    let tmp = std::env::temp_dir().join(format!("bn-resume-scope-{}", std::process::id()));
    let claude = tmp.join("claude-projects");
    let copilot = tmp.join("copilot-session-state");
    // Claude: `<projects>/<encoded-cwd>/<session>.jsonl`.
    std::fs::create_dir_all(claude.join("-tmp-bn-test")).unwrap();
    std::fs::write(claude.join("-tmp-bn-test").join("aaa.jsonl"), "{}\n").unwrap();
    // Copilot: `<session-state>/<session>/events.jsonl`.
    std::fs::create_dir_all(copilot.join("ccc")).unwrap();
    std::fs::write(copilot.join("ccc").join("events.jsonl"), "{}\n").unwrap();

    let backed = |harness, id| bn_core::agent::transcript_backed_in(&claude, &copilot, harness, id);
    assert!(backed("claude", "aaa"));
    assert!(backed("copilot", "ccc"));
    // Session ids are recorded in mixed case in places; the lookup canonicalizes.
    assert!(backed("claude", " AAA "));
    // The cases that used to resume into a blank prompt.
    assert!(!backed("claude", "bbb"));
    assert!(!backed("copilot", "aaa"));
    assert!(!backed("claude", ""));
    assert!(!backed("cursor", "aaa"));

    std::fs::remove_dir_all(&tmp).ok();
}

// --- where an agent may run -------------------------------------------------

/// Main hosts no agents *anywhere under it*. A root-only check let a real session at
/// `…/worktree/BigMono/main/packages/web-app` past every placement guard.
#[test]
fn main_is_off_limits_all_the_way_down() {
    use bn_core::agent::{is_main_worktree_path, is_within_main_worktree};
    use std::path::Path;

    let root = Path::new("/home/me/projects/worktree/BigMono/main");
    let deep = Path::new("/home/me/projects/worktree/BigMono/main/packages/web-app");

    // The narrow predicate still means exactly what its name says.
    assert!(is_main_worktree_path(root));
    assert!(!is_main_worktree_path(deep));

    // The placement rule covers the checkout and everything inside it.
    assert!(is_within_main_worktree(root));
    assert!(is_within_main_worktree(deep));
    assert!(is_within_main_worktree(Path::new(
        "/home/me/projects/worktree/bn/master/src"
    )));

    // Pool slots — where work actually happens — stay allowed, main slot included.
    assert!(!is_within_main_worktree(Path::new(
        "/home/me/projects/worktree/bn/_pool/1"
    )));
    assert!(!is_within_main_worktree(Path::new(
        "/home/me/projects/worktree/bn/_pool/2/crates"
    )));
    // A branch that merely starts with "main" is not main.
    assert!(!is_within_main_worktree(Path::new(
        "/home/me/projects/worktree/bn/maintenance"
    )));
    // And neither is a "main" dir that isn't laid out as …/worktree/<repo>/main.
    assert!(!is_within_main_worktree(Path::new("/home/me/src/bn/main")));
}
