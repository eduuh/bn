//! Golden-fixture harness for the **propose-don't-write** contract shared by `bn sync` (#101)
//! and `bn agent digest` (#96). Two invariants must never regress:
//!
//! 1. **proposals are correct** for a given repo/session state — pinned here as checked-in
//!    `--json` goldens (the exact `data` shape the CLI and MCP both emit, via `payload`);
//! 2. **nothing is written** without explicit confirm/`--yes` — the propose path is asserted
//!    byte-identical against the on-disk note.
//!
//! A third test drives the **apply** path (the mutations a confirmed proposal set lands) and
//! re-runs it to prove idempotence: a second sync over the now-updated note proposes nothing.
//!
//! Determinism comes from the existing seam — [`FakeRunner`] for git/gh reads, [`FixedClock`]
//! for "now" — so the goldens are stable across machines and time.

use bn_core::agent::{digest_sessions, AgentSession};
use bn_core::clock::FixedClock;
use bn_core::commands::{add, append_body, close, done};
use bn_core::resolver::{Config, Resolved};
use bn_core::runner::FakeRunner;
use bn_core::{payload, store, sync};
use serde_json::{json, Value};
use std::path::Path;

// --- fixtures ------------------------------------------------------------------------------

/// An isolated config rooted at a tempdir, matching the inline-test shape in `sync.rs`.
fn cfg(root: &Path) -> Config {
    Config {
        personal_root: root.to_path_buf(),
        work_root: root.to_path_buf(),
        worktree_dir: std::path::PathBuf::from("/nonexistent"),
        hostname: "testhost".into(),
        ..Default::default()
    }
}

fn ctx() -> Resolved {
    Resolved {
        repo: "bn".into(),
        branch: "feat/sync".into(),
    }
}

/// A note with two open todos created on a known date — the shared starting state for the
/// sync goldens. Mirrors `sync::tests::fixture_note`.
fn seed_two_todos(root: &Path) {
    let c = cfg(root);
    let x = ctx();
    add(&c, &x, "todo", "wire the resolver port", None, "2026-06-20").unwrap();
    add(
        &c,
        &x,
        "todo",
        "polish nvim cockpit colours",
        None,
        "2026-06-20",
    )
    .unwrap();
}

/// Register a `git log` fixture keyed on the marker the seeded note reconciles from.
fn fake_log(lines: &str) -> FakeRunner {
    FakeRunner::new().on(
        "git",
        &["log", "--since=2026-06-20", "--pretty=format:%h\x1f%s"],
        lines,
    )
}

/// A merged-PR `gh pr view` fixture.
fn with_merged_pr(r: FakeRunner, url: &str) -> FakeRunner {
    r.on(
        "gh",
        &["pr", "view", "--json", "state,url"],
        &format!(r#"{{"state":"MERGED","url":"{url}"}}"#),
    )
}

/// A runner for the post-apply re-sync: marker bumped to the apply date, no new commits, PR open.
fn empty_open_pr() -> FakeRunner {
    FakeRunner::new()
        .on(
            "git",
            &["log", "--since=2026-06-21", "--pretty=format:%h\x1f%s"],
            "",
        )
        .on(
            "gh",
            &["pr", "view", "--json", "state,url"],
            r#"{"state":"OPEN","url":"https://github.com/eduuh/bn/pull/1"}"#,
        )
}

/// An open-PR `gh pr view` fixture.
fn with_open_pr(r: FakeRunner) -> FakeRunner {
    r.on(
        "gh",
        &["pr", "view", "--json", "state,url"],
        r#"{"state":"OPEN","url":"https://github.com/eduuh/bn/pull/1"}"#,
    )
}

// --- (1) golden --json proposal sets for representative sync scenarios ----------------------

/// Each row: a named scenario, its `FakeRunner` git/gh state, and the exact `--json` payload
/// (`payload::sync_data`, the shape the CLI envelope and the MCP server both serialize). Pinning
/// the JSON — not just struct fields — is what guards the agent-facing contract from drift.
struct SyncCase {
    name: &'static str,
    runner: FakeRunner,
    golden: Value,
}

fn sync_cases() -> Vec<SyncCase> {
    vec![
        // commits → a progress line + a `bn done` candidate for the matched todo; no PR close.
        SyncCase {
            name: "commits_to_todos",
            runner: with_open_pr(fake_log(
                "abc123\x1ffeat(core): wire the resolver port end-to-end\n\
                 def456\x1fchore: bump deps",
            )),
            golden: json!({
                "proposals": {
                    "since": "2026-06-20",
                    "commits": [
                        { "hash": "abc123", "subject": "feat(core): wire the resolver port end-to-end" },
                        { "hash": "def456", "subject": "chore: bump deps" }
                    ],
                    "progress_line": "wire the resolver port end-to-end (2 commits)",
                    "done_candidates": [
                        {
                            "todo_id": "t1",
                            "todo_text": "wire the resolver port",
                            "commit": { "hash": "abc123", "subject": "feat(core): wire the resolver port end-to-end" }
                        }
                    ],
                    "close": null
                }
            }),
        },
        // merged PR → the close + `wt cleanup` proposal, alongside a single-commit progress line.
        SyncCase {
            name: "merged_pr_to_close",
            runner: with_merged_pr(
                fake_log("abc123\x1ffeat(core): wire the resolver port"),
                "https://github.com/eduuh/bn/pull/9",
            ),
            golden: json!({
                "proposals": {
                    "since": "2026-06-20",
                    "commits": [
                        { "hash": "abc123", "subject": "feat(core): wire the resolver port" }
                    ],
                    "progress_line": "wire the resolver port",
                    "done_candidates": [
                        {
                            "todo_id": "t1",
                            "todo_text": "wire the resolver port",
                            "commit": { "hash": "abc123", "subject": "feat(core): wire the resolver port" }
                        }
                    ],
                    "close": {
                        "pr_url": "https://github.com/eduuh/bn/pull/9",
                        "cleanup_slug": "bn/feat/sync"
                    }
                }
            }),
        },
        // already in sync: no commits, PR still open → an empty proposal set (silent exit).
        SyncCase {
            name: "in_sync_is_empty",
            runner: with_open_pr(fake_log("")),
            golden: json!({
                "proposals": {
                    "since": "2026-06-20",
                    "commits": [],
                    "progress_line": null,
                    "done_candidates": [],
                    "close": null
                }
            }),
        },
    ]
}

#[test]
fn sync_proposal_json_matches_golden() {
    let clock = FixedClock::at(1_750_000_000);
    for case in sync_cases() {
        let dir = tempfile::tempdir().unwrap();
        seed_two_todos(dir.path());
        let note = store::read(&cfg(dir.path()).note_file(&ctx())).unwrap();

        let p = sync::reconcile(&note, "bn", "feat/sync", &case.runner, &clock);
        let got = payload::sync_data(&p);
        assert_eq!(
            got, case.golden,
            "sync `--json` drifted from golden for scenario `{}`",
            case.name
        );
    }
}

// --- (2) no-write assertion: the propose path leaves the note byte-identical -----------------

#[test]
fn propose_path_leaves_note_byte_identical() {
    let clock = FixedClock::at(1_750_000_000);
    // Every scenario that *would* propose something must still write nothing.
    for case in sync_cases() {
        let dir = tempfile::tempdir().unwrap();
        seed_two_todos(dir.path());
        let note_path = cfg(dir.path()).note_file(&ctx());
        let note = store::read(&note_path).unwrap();
        let before = std::fs::read(&note_path).unwrap();

        let _ = sync::reconcile(&note, "bn", "feat/sync", &case.runner, &clock);

        let after = std::fs::read(&note_path).unwrap();
        assert_eq!(
            before, after,
            "reconcile wrote to the note for scenario `{}` — propose path must never write",
            case.name
        );
    }
}

// --- (3) apply path: lands exactly the proposed mutations, idempotently ----------------------

/// Apply a sync proposal set the way `bn sync --yes` would: append the progress bullet, mark
/// each `done` candidate done, and close the note on a merged-PR proposal. This is the test's
/// model of the confirmed-apply path — it composes the same `commands` the CLI calls.
fn apply(cfg: &Config, ctx: &Resolved, p: &sync::Proposals, today: &str) {
    if let Some(line) = &p.progress_line {
        append_body(cfg, ctx, "progress", line, today).unwrap();
    }
    for cand in &p.done_candidates {
        done(cfg, ctx, &cand.todo_id, today, None).unwrap();
    }
    if p.close.is_some() {
        close(cfg, ctx, today, None).unwrap();
    }
}

#[test]
fn apply_commits_lands_progress_and_selective_done_idempotently() {
    let dir = tempfile::tempdir().unwrap();
    let c = cfg(dir.path());
    let x = ctx();
    seed_two_todos(dir.path());
    let clock = FixedClock::at(1_750_000_000);

    // A commit that closes the resolver todo, PR still open → progress + one done, no close.
    let runner = with_open_pr(fake_log("abc123\x1ffeat(core): wire the resolver port"));

    let note = store::read(&c.note_file(&x)).unwrap();
    let p = sync::reconcile(&note, "bn", "feat/sync", &runner, &clock);
    assert!(p.close.is_none(), "open PR must not propose a close");

    apply(&c, &x, &p, "2026-06-21");

    // Exactly the proposed mutations landed — no more, no less:
    let applied = store::read(&c.note_file(&x)).unwrap();
    // (a) the matched todo is now done…
    let t1 = applied.front.todos.iter().find(|t| t.id == "t1").unwrap();
    assert_eq!(t1.done, Some(true), "matched todo should be closed");
    // (b) …the unmatched one is untouched (a selective done, not a blanket close)…
    let t2 = applied.front.todos.iter().find(|t| t.id == "t2").unwrap();
    assert!(t2.is_open_todo(), "unmatched todo must stay open");
    // (c) …the progress line was appended…
    assert!(
        applied.body.contains("wire the resolver port"),
        "progress bullet should be in the body:\n{}",
        applied.body
    );
    // (d) …and the note stays open (no merged PR).
    assert_ne!(
        applied.front.status, "closed",
        "open PR must not close note"
    );

    // Idempotence: re-run sync over the now-updated note. The marker is bumped to the apply date
    // and there are no new commits, so nothing further is proposed.
    let again = sync::reconcile(&applied, "bn", "feat/sync", &empty_open_pr(), &clock);
    assert!(
        again.is_empty(),
        "a re-sync after apply must propose nothing"
    );
}

#[test]
fn apply_merged_pr_closes_the_note() {
    let dir = tempfile::tempdir().unwrap();
    let c = cfg(dir.path());
    let x = ctx();
    seed_two_todos(dir.path());
    let clock = FixedClock::at(1_750_000_000);

    let runner = with_merged_pr(
        fake_log("abc123\x1ffeat(core): wire the resolver port"),
        "https://github.com/eduuh/bn/pull/9",
    );
    let note = store::read(&c.note_file(&x)).unwrap();
    let p = sync::reconcile(&note, "bn", "feat/sync", &runner, &clock);
    let close = p.close.as_ref().expect("merged PR should propose a close");
    assert_eq!(close.cleanup_slug, "bn/feat/sync");

    apply(&c, &x, &p, "2026-06-21");

    // The merged-PR close proposal closes the note (and, per `close` semantics, all open todos).
    let applied = store::read(&c.note_file(&x)).unwrap();
    assert_eq!(
        applied.front.status, "closed",
        "merged PR should close note"
    );
    assert!(
        applied.front.todos.iter().all(|t| !t.is_open_todo()),
        "close must mark every open todo done"
    );
}

// --- (4) #96 agent digest shares the contract: golden JSON + no-write -----------------------

/// A session ending a day after the note was created (so it's uncovered), with `lines`/`files`
/// of code change. Mirrors `agent::tests::session`.
fn session(id: &str, harness: &str, start: i64, end: i64, lines: i64, files: i64) -> AgentSession {
    AgentSession {
        session_id: id.into(),
        harness: harness.into(),
        repo: "bn".into(),
        branch: "feat/sync".into(),
        started_ts: start,
        ended_ts: end,
        turns: 5,
        tool_calls: 12,
        permission_prompts: 0,
        models: vec!["claude-opus-4-8".into()],
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cost_nanoaiu: None,
        lines_added: lines,
        lines_removed: 0,
        files_modified: files,
        outcome: "completed".into(),
        work_class: bn_core::cost::WorkClass::Unattributed,
    }
}

#[test]
fn agent_digest_json_matches_golden_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let note_path = cfg(dir.path()).note_file(&ctx());

    // A code-changing copilot session ending a day after the note was created → uncovered.
    // Deriving the created marker from the session ts (as the inline agent tests do) keeps the
    // "is it covered?" date comparison independent of wall-clock.
    let start = 1_750_086_400;
    let created = bn_core::util::date_of(start - 86_400); // the day before the session
    add(&cfg(dir.path()), &ctx(), "todo", "anything", None, &created).unwrap();
    let note = store::read(&note_path).unwrap();
    let before = std::fs::read(&note_path).unwrap();

    // +30/-0 across 2 files → progress + decision + todo.
    let s = session("s1", "copilot", start, start + 40 * 60, 30, 2);

    let d = digest_sessions(&note, &[s]);
    let got = payload::agent_digest_data(&d);

    // Pin the digest `--json` shape: the marker, uncovered count, and the three proposal kinds
    // with their session-derived metadata. (Progress text carries the +/- diff summary.)
    let kinds: Vec<&str> = d.proposals.iter().map(|p| p.kind.section()).collect();
    assert_eq!(
        kinds,
        vec!["progress", "decision", "todo"],
        "code session → progress + decision + todo"
    );
    assert_eq!(got["digest"]["since"], json!(d.since));
    assert_eq!(got["digest"]["sessions"], json!(1));
    assert_eq!(got["digest"]["proposals"].as_array().unwrap().len(), 3);
    for prop in got["digest"]["proposals"].as_array().unwrap() {
        assert_eq!(prop["session_id"], json!("s1"));
        assert_eq!(prop["harness"], json!("copilot"));
        assert_eq!(prop["duration_mins"], json!(40));
    }

    // Same no-write invariant as sync: digesting reads sessions, never touches the note.
    let after = std::fs::read(&note_path).unwrap();
    assert_eq!(
        before, after,
        "agent digest must not write the note (propose-don't-write)"
    );
}
