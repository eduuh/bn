//! Black-box migration validators, ported from `scripts/validate-*.sh` to native Rust integration
//! tests (Phase 3b of the .sh→Rust migration). Each drives the built `bn` binary
//! (`CARGO_BIN_EXE_bn`) against an isolated `BN_NOTES_DIR` sandbox and asserts the observable
//! outcome — the same contract the shell harnesses checked, now in the fast `cargo test` lane with
//! no external deps (jq/bash/oracle). The oracle-parity halves of the old scripts are intentionally
//! dropped: the Rust binary is the source of truth and MIGRATION.md documents the deliberate
//! deviations.

use std::path::Path;
use std::process::Command;

/// The built `bn` binary under test, with the shared sandbox env applied. `notes` is the
/// `BN_NOTES_DIR` root; `wt` the `BN_WORKTREE_DIR`; `today` pins `BN_TODAY` for date-sensitive
/// commands. Hostname is always `h` so the machine subtree is `<notes>/h/<repo>/<branch>`.
fn bn(notes: &Path, wt: &Path, today: &str) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_bn"));
    c.env("BN_NOTES_DIR", notes)
        .env("BN_WORKTREE_DIR", wt)
        .env("BN_HOSTNAME", "h")
        .env("BN_TODAY", today);
    c
}

/// Seed a minimal single-file note (frontmatter only) at `dir` with the given `status`/`created`,
/// mirroring the `mk_note` helper in the shell validators.
fn mk_note(dir: &Path, status: &str, created: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let body = format!(
        "---\nrepo: r\nbranch: b\ncreated: {created}\nstatus: {status}\nmachine: h\ntags: []\n\
         collaborators: []\ntodos: []\nblockers: []\ndecisions: []\nresearch: []\ncollab: []\n\
         ask: []\n---\n# r / b\n\n## Goal\n\n\n"
    );
    std::fs::write(dir.join("note.md"), body).unwrap();
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

/// A throwaway git worktree at `<wt>/<repo>/<branch>` on `branch`, with a committer identity set
/// so `autopush` and friends can commit. Mirrors `mk_wt` in the shell validators. Returns the
/// worktree path.
fn mk_wt(wt: &Path, repo: &str, branch: &str) -> std::path::PathBuf {
    let dir = wt.join(repo).join(branch);
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&dir)
                .status()
                .unwrap()
                .success(),
            "git {args:?} failed"
        );
    };
    git(&["init", "-q"]);
    git(&["checkout", "-q", "-b", branch]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    dir
}

/// Ported from `validate-note-atomic.sh`: N *separate* `bn` processes doing read-modify-write on
/// ONE note at once must all survive (no lost update), leave the note parseable (no torn write),
/// and drop no `.tmp`/`.lock` scratch files.
#[test]
fn note_atomic_concurrent_adds_survive() {
    let tmp = tempfile::tempdir().unwrap();
    let notes = tmp.path().join("notes");
    let wt = tmp.path().join("wt");
    std::fs::create_dir_all(&notes).unwrap();
    let notedir = notes.join("h").join("sandbox").join("main");

    // Seed so every writer read-modify-writes the same existing file.
    assert!(bn(&notes, &wt, "2026-06-20")
        .args(["add", "todo", "seed", "--repo", "sandbox", "--branch", "main"])
        .status()
        .unwrap()
        .success());

    const N: usize = 25;
    let handles: Vec<_> = (0..N)
        .map(|i| {
            let (notes, wt) = (notes.clone(), wt.clone());
            std::thread::spawn(move || {
                bn(&notes, &wt, "2026-06-20")
                    .args([
                        "add",
                        "todo",
                        &format!("concurrent-{i}"),
                        "--repo",
                        "sandbox",
                        "--branch",
                        "main",
                    ])
                    .status()
                    .unwrap()
                    .success()
            })
        })
        .collect();
    for h in handles {
        assert!(h.join().unwrap(), "a concurrent add failed");
    }

    let note = read(&notedir.join("note.md"));
    let got = note.matches("concurrent-").count();
    assert_eq!(
        got, N,
        "all {N} concurrent adds must survive (no lost update)"
    );

    // A follow-up add must READ the note first — if the storm left it torn, this fails.
    assert!(
        bn(&notes, &wt, "2026-06-20")
            .args([
                "add",
                "decision",
                "post-storm",
                "--repo",
                "sandbox",
                "--branch",
                "main"
            ])
            .status()
            .unwrap()
            .success(),
        "note must still parse after the storm"
    );

    let leaked: Vec<_> = std::fs::read_dir(&notedir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.ends_with(".tmp") || n.ends_with(".lock")
        })
        .collect();
    assert!(
        leaked.is_empty(),
        "no .tmp/.lock scratch files may leak: {leaked:?}"
    );
}

/// Ported from `validate-lifecycle.sh` (archive): an old closed note moves to
/// `archive/<repo>/<branch>`; a recently-closed one stays in the active tree.
#[test]
fn lifecycle_archive_moves_old_closed_keeps_recent() {
    let tmp = tempfile::tempdir().unwrap();
    let notes = tmp.path().join("notes");
    let wt = tmp.path().join("wt");

    mk_note(&notes.join("h/acme/feat"), "closed", "2026-01-01");
    assert!(bn(&notes, &wt, "2026-06-20")
        .args(["archive", "--days", "30"])
        .status()
        .unwrap()
        .success());
    assert!(
        notes.join("archive/acme/feat/note.md").is_file(),
        "old closed note archived"
    );
    assert!(
        !notes.join("h/acme/feat").is_dir(),
        "source removed after archive"
    );

    // Recency guard: a recently-closed note stays put.
    mk_note(&notes.join("h/acme/recent"), "closed", "2026-06-15");
    assert!(bn(&notes, &wt, "2026-06-20")
        .args(["archive", "--days", "30"])
        .status()
        .unwrap()
        .success());
    assert!(
        notes.join("h/acme/recent").is_dir(),
        "recent note kept in active tree"
    );
    assert!(
        !notes.join("archive/acme/recent").is_dir(),
        "recent note not archived"
    );
}

/// Ported from `validate-lifecycle.sh` (prune): a worktree-managed repo (bare exists) whose
/// worktree is gone → its note is closed; a live worktree stays active; a repo with no bare clone
/// is never pruned.
#[test]
fn lifecycle_prune_closes_only_missing_managed_worktrees() {
    let tmp = tempfile::tempdir().unwrap();
    let notes = tmp.path().join("notes");
    let wt = tmp.path().join("wt");

    mk_note(&notes.join("h/acme/gone"), "active", "2026-06-01");
    std::fs::create_dir_all(tmp.path().join("bare/acme.git")).unwrap(); // managed
    mk_note(&notes.join("h/acme/live"), "active", "2026-06-01");
    std::fs::create_dir_all(wt.join("acme/live")).unwrap(); // still has a worktree

    assert!(bn(&notes, &wt, "2026-06-20")
        .arg("prune")
        .status()
        .unwrap()
        .success());
    assert!(
        read(&notes.join("h/acme/gone/note.md")).contains("status: closed"),
        "missing-worktree managed note closed"
    );
    assert!(
        read(&notes.join("h/acme/live/note.md")).contains("status: active"),
        "live-worktree note left active"
    );

    // A repo with no bare clone is never pruned even with no worktree.
    mk_note(&notes.join("h/solo/main"), "active", "2026-06-01");
    assert!(bn(&notes, &wt, "2026-06-20")
        .arg("prune")
        .status()
        .unwrap()
        .success());
    assert!(
        read(&notes.join("h/solo/main/note.md")).contains("status: active"),
        "non-worktree repo left active"
    );
}

/// Ported from `validate-lifecycle.sh` (stale): notes idle past `--days` are listed (by
/// activity date, the documented deviation), fresh ones aren't, and `--json` wraps a clean
/// envelope with exactly the stale set.
#[test]
fn lifecycle_stale_lists_idle_by_activity_date() {
    let tmp = tempfile::tempdir().unwrap();
    let notes = tmp.path().join("notes");
    let wt = tmp.path().join("wt");

    mk_note(&notes.join("h/acme/old"), "active", "2026-01-01");
    mk_note(&notes.join("h/acme/fresh"), "active", "2026-06-19");

    let out = bn(&notes, &wt, "2026-06-20")
        .args(["stale", "--days", "14"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("acme/old"), "old note listed:\n{text}");
    assert!(
        !text.contains("acme/fresh"),
        "fresh note not listed:\n{text}"
    );

    let json_out = bn(&notes, &wt, "2026-06-20")
        .args(["stale", "--days", "14", "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value =
        serde_json::from_slice(&json_out.stdout).expect("stale --json emits valid JSON");
    assert_eq!(v["ok"], serde_json::json!(true), "envelope ok");
    assert_eq!(
        v["data"]["stale"].as_array().map(|a| a.len()),
        Some(1),
        "exactly one stale note"
    );
}

/// Ported from `validate-pr-link.sh` (Rust side): `link <id>` writes `work_item` to frontmatter and
/// prints a confirmation; bare `link` reads back the stored id; `pr <url>` records and reads back
/// the PR. Runs inside a throwaway git worktree so repo/branch resolve from cwd.
#[test]
fn pr_link_set_show_and_json() {
    let tmp = tempfile::tempdir().unwrap();
    let notes = tmp.path().join("notes");
    let wt = tmp.path().join("wt");
    let dir = mk_wt(&wt, "acme", "main");
    let run = |args: &[&str]| -> std::process::Output {
        bn(&notes, &wt, "2026-06-20")
            .current_dir(&dir)
            .args(args)
            .output()
            .unwrap()
    };
    let so = |o: std::process::Output| String::from_utf8_lossy(&o.stdout).trim().to_string();

    assert_eq!(so(run(&["link", "4242"])), "Work item linked: 4242");
    assert!(
        read(&notes.join("h/acme/main/note.md")).contains("work_item: 4242"),
        "work_item written to frontmatter"
    );
    assert_eq!(
        so(run(&["link"])),
        "Work item: 4242",
        "bare link reads back the stored id"
    );
    let v: serde_json::Value = serde_json::from_slice(&run(&["link", "--json"]).stdout).unwrap();
    assert_eq!(v["ok"], serde_json::json!(true));
    assert_eq!(v["data"]["url"], serde_json::json!("4242"));

    let pr_url = "https://github.com/acme/x/pull/7";
    assert_eq!(so(run(&["pr", pr_url])), format!("PR linked: {pr_url}"));
    assert!(
        read(&notes.join("h/acme/main/note.md")).contains("pr: "),
        "pr written to frontmatter"
    );
    assert_eq!(
        so(run(&["pr", "--show"])),
        pr_url,
        "--show prints stored PR url"
    );
}

/// Ported from `validate-notes-autopush.sh` (Rust side): notes/tc append to independent body
/// sections and list in order (with a `--json` envelope); log-progress writes a timestamped bullet
/// under `## Progress`; autopush reports a clean tree, then commits a tracked change (no upstream).
#[test]
fn notes_tc_logprogress_autopush() {
    let tmp = tempfile::tempdir().unwrap();
    let notes = tmp.path().join("notes");
    let wt = tmp.path().join("wt");
    let dir = mk_wt(&wt, "acme", "main");
    let run = |args: &[&str]| -> std::process::Output {
        bn(&notes, &wt, "2026-06-20")
            .current_dir(&dir)
            .args(args)
            .output()
            .unwrap()
    };
    let so = |o: std::process::Output| String::from_utf8_lossy(&o.stdout).trim().to_string();

    run(&["notes", "first note"]);
    run(&["notes", "second note"]);
    run(&["tc", "login works"]);
    assert_eq!(
        so(run(&["notes"])),
        "- first note\n- second note",
        "notes list in order"
    );
    assert_eq!(so(run(&["tc"])), "- login works", "tc independent of notes");
    assert!(
        so(run(&["notes", "third"])).contains("Added to Notes (main): third"),
        "notes append confirmation"
    );
    let v: serde_json::Value = serde_json::from_slice(&run(&["notes", "--json"]).stdout).unwrap();
    assert_eq!(v["ok"], serde_json::json!(true));
    assert_eq!(
        v["data"]["lines"].as_array().map(|a| a.len()),
        Some(3),
        "3 note lines"
    );

    // log-progress: timestamped bullet under ## Progress.
    bn(&notes, &wt, "2026-06-20")
        .env("BN_NOW", "1750000000")
        .current_dir(&dir)
        .args(["log-progress", "did the thing"])
        .status()
        .unwrap();
    let note = read(&notes.join("h/acme/main/note.md"));
    let progress: String = note
        .lines()
        .skip_while(|l| !l.starts_with("## Progress"))
        .take_while(|l| l.starts_with("## Progress") || !l.starts_with("## "))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        progress.contains("did the thing"),
        "bullet under ## Progress:\n{note}"
    );
    let ts = regex_like_progress_line(&note);
    assert!(
        ts,
        "progress line is timestamped `- YYYY-MM-DD HH:MM — text`:\n{note}"
    );

    // autopush: clean tree, then a tracked change committed with no upstream.
    let commit = |m: &str| {
        for a in [vec!["add", "-A"], vec!["commit", "-qm", m]] {
            Command::new("git")
                .args(&a)
                .current_dir(&dir)
                .status()
                .unwrap();
        }
    };
    std::fs::write(dir.join("f.txt"), "a\n").unwrap();
    commit("init");
    assert_eq!(
        so(run(&["autopush"])),
        "No tracked changes to commit.",
        "clean tree reports nothing"
    );
    std::fs::write(dir.join("f.txt"), "a\nb\n").unwrap();
    assert!(
        so(run(&["autopush", "my message"])).contains("No upstream"),
        "no-upstream message"
    );
    let last = String::from_utf8_lossy(
        &Command::new("git")
            .args(["log", "-1", "--pretty=%s"])
            .current_dir(&dir)
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    assert_eq!(last, "my message", "autopush commit created with message");
}

/// True when the note has a `## Progress` bullet of the form `- YYYY-MM-DD HH:MM — <text>` — the
/// timestamp-format assertion from the shell harness, without pulling in a regex crate.
fn regex_like_progress_line(note: &str) -> bool {
    note.lines().any(|l| {
        let Some(rest) = l.strip_prefix("- ") else {
            return false;
        };
        let bytes = rest.as_bytes();
        // "YYYY-MM-DD HH:MM — " prefix: positions of digits and separators.
        bytes.len() >= 16
            && bytes[..4].iter().all(u8::is_ascii_digit)
            && bytes[4] == b'-'
            && bytes[5..7].iter().all(u8::is_ascii_digit)
            && bytes[7] == b'-'
            && bytes[8..10].iter().all(u8::is_ascii_digit)
            && bytes[10] == b' '
            && bytes[11..13].iter().all(u8::is_ascii_digit)
            && bytes[13] == b':'
            && bytes[14..16].iter().all(u8::is_ascii_digit)
            && rest.contains(" — ")
    })
}

// ---------------------------------------------------------------------------
// Bare-repo worktree topology (ported from validate-recover / reset-rename /
// wt-cleanup). Unlike `mk_wt` above, these need a real origin + `main` baseline
// so "commits ahead", branch refs, and PR-merge reclaim semantics are exercised.
// ---------------------------------------------------------------------------

/// Run `git <args>` in `dir`, asserting success. Used only by the bare-repo topology helpers.
fn git_in(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} in {dir:?} failed");
}

/// A sandbox bare repo `<tmp>/projects/bare/<repo>.git` cloned from a one-commit `main` seed,
/// plus the notes/worktree roots. Returns `(notes, wt_root, bare)`. Mirrors the sandbox block
/// shared by the recover/reset-rename/wt-cleanup shell validators.
fn bare_sandbox(
    tmp: &Path,
    repo: &str,
) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let notes = tmp.join("notes");
    let wt_root = tmp.join("projects").join("worktree");
    let bare = tmp
        .join("projects")
        .join("bare")
        .join(format!("{repo}.git"));
    let seed = tmp.join("seed");
    std::fs::create_dir_all(tmp.join("projects").join("bare")).unwrap();
    std::fs::create_dir_all(&wt_root).unwrap();
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::create_dir_all(&seed).unwrap();
    git_in(&seed, &["init", "-q"]);
    git_in(&seed, &["commit", "-q", "--allow-empty", "-m", "init"]);
    git_in(&seed, &["branch", "-M", "main"]);
    git_in(
        tmp,
        &[
            "clone",
            "-q",
            "--bare",
            seed.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    (notes, wt_root, bare)
}

/// `git --git-dir=<bare> worktree add <wt_root>/<repo>/<branch> -b <branch> main`.
fn bare_wt_add(bare: &Path, wt_root: &Path, repo: &str, branch: &str) -> std::path::PathBuf {
    let dir = wt_root.join(repo).join(branch);
    git_in(
        bare,
        &[
            "--git-dir",
            bare.to_str().unwrap(),
            "worktree",
            "add",
            "-q",
            dir.to_str().unwrap(),
            "-b",
            branch,
            "main",
        ],
    );
    dir
}

/// Like `bare_wt_add` but checks out an *existing* branch (no `-b`), e.g. `main`.
fn bare_wt_checkout(bare: &Path, wt_root: &Path, repo: &str, branch: &str) -> std::path::PathBuf {
    let dir = wt_root.join(repo).join(branch);
    git_in(
        bare,
        &[
            "--git-dir",
            bare.to_str().unwrap(),
            "worktree",
            "add",
            "-q",
            dir.to_str().unwrap(),
            branch,
        ],
    );
    dir
}

fn bare_branch_exists(bare: &Path, branch: &str) -> bool {
    Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .status()
        .unwrap()
        .success()
}

/// Ported from `validate-recover.sh`: `bn agent recover --all` (dry run) flags only worktrees with
/// commits ahead of base AND no recorded PR as "would-recover" candidates; a worktree with a PR or
/// with no commits ahead is skipped. (The `--yes` push+PR path needs a real remote+gh, so — as in
/// the shell harness — it is not exercised here.)
#[test]
fn recover_flags_stranded_ship_turns_only() {
    let tmp = tempfile::tempdir().unwrap();
    let (notes, wt_root, bare) = bare_sandbox(tmp.path(), "sandbox");
    let commit_in = |dir: &Path| {
        std::fs::write(dir.join("f"), "x").unwrap();
        git_in(dir, &["add", "-A"]);
        git_in(dir, &["commit", "-q", "-m", "work"]);
    };

    let stranded = bare_wt_add(&bare, &wt_root, "sandbox", "feat-stranded");
    commit_in(&stranded); // commits ahead, no PR -> candidate

    let haspr = bare_wt_add(&bare, &wt_root, "sandbox", "feat-haspr");
    commit_in(&haspr); // commits ahead, HAS PR -> skip
    assert!(bn(&notes, &wt_root, "2026-06-20")
        .current_dir(&haspr)
        .args(["pr", "https://github.com/o/r/pull/5"])
        .status()
        .unwrap()
        .success());

    bare_wt_add(&bare, &wt_root, "sandbox", "feat-clean"); // no commits ahead -> skip

    let out = bn(&notes, &wt_root, "2026-06-20")
        .args(["--json", "agent", "recover", "--all"])
        .output()
        .unwrap();
    assert!(out.status.success(), "recover --json exits ok");
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("recover emits valid JSON");
    let results = v["data"]["results"].as_array().cloned().unwrap_or_default();
    let cand = |branch: &str| {
        results
            .iter()
            .find(|r| r["branch"] == serde_json::json!(branch))
    };

    let s = cand("feat-stranded").expect("stranded worktree is a candidate");
    assert_eq!(
        s["action"],
        serde_json::json!("would-recover"),
        "stranded action"
    );
    assert!(
        s["commits_ahead"].as_i64().unwrap_or(0) >= 1,
        "stranded reports commits_ahead >= 1"
    );
    assert!(
        cand("feat-haspr").is_none(),
        "worktree WITH a PR is skipped"
    );
    assert!(
        cand("feat-clean").is_none(),
        "worktree with no commits is skipped"
    );
    assert_eq!(
        v["data"]["candidates"],
        serde_json::json!(1),
        "exactly 1 candidate"
    );
}

/// Ported from `validate-reset-rename.sh`: `bn reset` (dry run lists, removes nothing), `bn reset
/// --force` (feature worktrees gone, `main` preserved, notes closed, local branch refs retained),
/// and `bn rename <new>` (branch renamed, note folder moved, frontmatter `branch:` rewritten, plus
/// the invalid-name / existing-branch / rename-to-self guards).
#[test]
fn reset_and_rename_worktree_note_semantics() {
    let tmp = tempfile::tempdir().unwrap();
    let (notes, wt_root, bare) = bare_sandbox(tmp.path(), "sandbox");
    let note_dir = notes.join("h").join("sandbox");
    let present = |b: &str| wt_root.join("sandbox").join(b).is_dir();
    let note_status = |b: &str| {
        read(&note_dir.join(b).join("note.md"))
            .lines()
            .find_map(|l| l.strip_prefix("status:").map(|s| s.trim().to_string()))
            .unwrap_or_default()
    };
    let add_todo = |dir: &Path, text: &str| {
        assert!(bn(&notes, &wt_root, "2026-06-20")
            .current_dir(dir)
            .args(["add", "todo", text])
            .status()
            .unwrap()
            .success());
    };

    bare_wt_checkout(&bare, &wt_root, "sandbox", "main");
    let one = bare_wt_add(&bare, &wt_root, "sandbox", "feat-one");
    add_todo(&one, "work on feat-one");
    let two = bare_wt_add(&bare, &wt_root, "sandbox", "feat-two");
    add_todo(&two, "work on feat-two");

    // reset dry run: lists but removes nothing.
    let dry = bn(&notes, &wt_root, "2026-06-20")
        .current_dir(&one)
        .arg("reset")
        .output()
        .unwrap();
    let dry_out = String::from_utf8_lossy(&dry.stdout) + String::from_utf8_lossy(&dry.stderr);
    assert!(dry_out.contains("feat-one"), "dry run lists feat-one");
    assert!(dry_out.contains("feat-two"), "dry run lists feat-two");
    assert!(
        present("feat-one") && present("feat-two") && present("main"),
        "dry run removes nothing"
    );

    // reset --force: features gone, main preserved, notes closed, branch refs retained.
    bn(&notes, &wt_root, "2026-06-20")
        .current_dir(wt_root.join("sandbox").join("main"))
        .args(["reset", "--force"])
        .status()
        .unwrap();
    assert!(
        !present("feat-one") && !present("feat-two"),
        "force removed features"
    );
    assert!(present("main"), "force preserved main");
    assert!(
        bare_branch_exists(&bare, "feat-one"),
        "feat-one branch retained"
    );
    assert_eq!(note_status("feat-one"), "closed", "feat-one note closed");
    assert_eq!(note_status("feat-two"), "closed", "feat-two note closed");

    // rename: branch + note folder + frontmatter.
    let old = bare_wt_add(&bare, &wt_root, "sandbox", "old-feat");
    add_todo(&old, "before rename");
    assert!(bn(&notes, &wt_root, "2026-06-20")
        .current_dir(&old)
        .args(["rename", "new-feat"])
        .status()
        .unwrap()
        .success());
    assert!(bare_branch_exists(&bare, "new-feat"), "git branch renamed");
    assert!(
        !bare_branch_exists(&bare, "old-feat"),
        "old git branch gone"
    );
    let cur = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(&old)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&cur.stdout).trim(),
        "new-feat",
        "worktree on new-feat"
    );
    assert!(
        note_dir.join("new-feat").join("note.md").is_file(),
        "note folder moved"
    );
    assert!(!note_dir.join("old-feat").is_dir(), "old note folder gone");
    assert!(
        read(&note_dir.join("new-feat").join("note.md")).contains("\nbranch: new-feat\n"),
        "frontmatter branch rewritten"
    );

    // guards.
    let invalid = bn(&notes, &wt_root, "2026-06-20")
        .current_dir(&old)
        .args(["rename", "bad name"])
        .status()
        .unwrap();
    assert!(!invalid.success(), "invalid name rejected");
    let selfrename = bn(&notes, &wt_root, "2026-06-20")
        .current_dir(&old)
        .args(["rename", "new-feat"])
        .status()
        .unwrap();
    assert!(selfrename.success(), "rename-to-self is a no-op (ok)");
    bare_wt_add(&bare, &wt_root, "sandbox", "taken");
    let existing = bn(&notes, &wt_root, "2026-06-20")
        .current_dir(&old)
        .args(["rename", "taken"])
        .status()
        .unwrap();
    assert!(!existing.success(), "existing branch rejected");
    assert!(
        bare_branch_exists(&bare, "taken"),
        "taken still its own branch"
    );
}

/// Ported from `validate-wt-cleanup.sh`: the merge→reclaim sweep and its guards, using a fake `gh`
/// (a tiny `/bin/sh` stub on PATH) that scripts each branch's PR state. Asserts the observable
/// outcome — merged+clean reclaimed (worktree + branch gone); merged+dirty kept with exactly ONE
/// idempotent `wt-stuck` attention item; merged+open-todo and open-PR kept; a commit past the
/// merged head is guarded (kept) until `--force`, which reclaims it and resolves the stuck item.
#[test]
fn wt_cleanup_reclaims_merged_guards_dirty_and_open() {
    let tmp = tempfile::tempdir().unwrap();
    let (notes, wt_root, bare) = bare_sandbox(tmp.path(), "sandbox");
    // A GitHub origin so cleanup performs PR lookups (via the fake gh).
    git_in(
        &bare,
        &[
            "--git-dir",
            bare.to_str().unwrap(),
            "remote",
            "set-url",
            "origin",
            "https://github.com/eduuh/sandbox.git",
        ],
    );

    let state_dir = tmp.path().join("state");
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let events = notes.join("h").join("state").join("events.jsonl");

    // Fake gh: models the two queries `wt cleanup` makes — `--json state` returns the scripted PR
    // state for the --head branch (missing file → NONE via exit 0), and `--json headRefOid` returns
    // the merged PR's head sha (= the `main` baseline) but only for a MERGED branch.
    let gh = bin.join("gh");
    std::fs::write(
        &gh,
        r#"#!/bin/sh
head=""; json=""
while [ $# -gt 0 ]; do
  case "$1" in
    --head) head="$2"; shift ;;
    --json) json="$2"; shift ;;
  esac
  shift
done
[ -n "$head" ] && [ -f "$STATE_DIR/$head" ] || exit 0
state=$(cat "$STATE_DIR/$head")
case "$json" in
  *headRefOid*) [ "$state" = MERGED ] && git --git-dir="$BARE" rev-parse refs/heads/main ;;
  *) printf '[{"state":"%s"}]\n' "$state" ;;
esac
exit 0
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path_env = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    // `bn` with the sandbox env plus PATH (fake gh), STATE_DIR and BARE (read by the stub).
    let cleanup_bn = |dir: Option<&Path>| {
        let mut c = bn(&notes, &wt_root, "2026-06-20");
        c.env("PATH", &path_env)
            .env("STATE_DIR", &state_dir)
            .env("BARE", &bare);
        if let Some(d) = dir {
            c.current_dir(d);
        }
        c
    };

    let mk_wt = |branch: &str, pr_state: &str| -> std::path::PathBuf {
        let dir = bare_wt_add(&bare, &wt_root, "sandbox", branch);
        std::fs::write(state_dir.join(branch), format!("{pr_state}\n")).unwrap();
        dir
    };
    let present = |b: &str| wt_root.join("sandbox").join(b).is_dir();
    let attn_raises = |b: &str| {
        read(&events)
            .matches(&format!("\"key\":\"wt-stuck:sandbox/{b}\""))
            .count()
    };
    let attn_open = |b: &str| {
        let out = cleanup_bn(None).arg("attn").output().unwrap();
        String::from_utf8_lossy(&out.stdout).contains(&format!("sandbox/{b}"))
    };

    let clean = mk_wt("feat-clean", "MERGED");
    let dirty = mk_wt("feat-dirty", "MERGED");
    let open_todo = mk_wt("feat-open", "MERGED");
    mk_wt("feat-openpr", "OPEN");
    // dirty: untracked file blocks `git worktree remove`.
    std::fs::write(dirty.join("scratch.txt"), "").unwrap();
    // open todo on an otherwise-clean merged worktree.
    assert!(cleanup_bn(Some(&open_todo))
        .args(["add", "todo", "unfinished"])
        .status()
        .unwrap()
        .success());
    let _ = clean;

    // run 1: the sweep.
    cleanup_bn(None)
        .args(["wt", "cleanup", "--all"])
        .status()
        .unwrap();
    assert!(!present("feat-clean"), "merged+clean reclaimed");
    assert!(
        !bare_branch_exists(&bare, "feat-clean"),
        "merged+clean branch deleted"
    );
    assert!(present("feat-dirty"), "merged+dirty kept");
    assert_eq!(attn_raises("feat-dirty"), 1, "merged+dirty raised 1 attn");
    assert!(attn_open("feat-dirty"), "merged+dirty attn is open");
    assert!(present("feat-open"), "merged+open-todo kept");
    assert!(present("feat-openpr"), "open-PR kept");

    // run 2: re-sweep dedups — no second attention item.
    cleanup_bn(None)
        .args(["wt", "cleanup", "--all"])
        .status()
        .unwrap();
    assert!(present("feat-dirty"), "dirty still kept");
    assert_eq!(attn_raises("feat-dirty"), 1, "no duplicate attn (still 1)");

    // run 3: commit past the merged head — cleanup refuses to auto-delete (guards the unpushed commit).
    git_in(&dirty, &["add", "-A"]);
    git_in(&dirty, &["commit", "-q", "-m", "scratch"]);
    cleanup_bn(None)
        .args(["wt", "cleanup", "--all"])
        .status()
        .unwrap();
    assert!(
        present("feat-dirty"),
        "commit-past-merge kept (needs --force)"
    );
    assert!(attn_open("feat-dirty"), "stuck attn still open");

    // run 4: --force reclaims and resolves the stuck item.
    cleanup_bn(Some(&dirty))
        .args(["wt", "cleanup", "--force"])
        .status()
        .unwrap();
    assert!(!present("feat-dirty"), "force reclaims committed worktree");
    assert!(
        !attn_open("feat-dirty"),
        "stuck attn resolved on force cleanup"
    );
}

// ---------------------------------------------------------------------------
// Toast-job fixture (ported from validate-standup / validate-wrap). Both jobs
// (`bn run standup-push` / `bn run wrap-digest`) reach for git/gh/notify-send on
// PATH; we pin exit-1 git/gh stubs and a notify-send stub that appends one
// `CALL<TAB>argv…` line per invocation to a log, so payload + call COUNT are
// both assertable, and drive the run from a non-repo cwd off seeded daily state.
// ---------------------------------------------------------------------------

/// Isolated toast-job sandbox: notes/wt/cfg/run trees plus a bin dir holding exit-1 `git`/`gh` and
/// a payload-logging `notify-send`. All paths hang off `tmp`.
struct JobFixture {
    notes: std::path::PathBuf,
    wt: std::path::PathBuf,
    cfg: std::path::PathBuf,
    run: std::path::PathBuf,
    log: std::path::PathBuf,
    path_env: String,
}

fn job_fixture(tmp: &Path) -> JobFixture {
    let notes = tmp.join("notes");
    let wt = tmp.join("wt");
    let cfg = tmp.join("cfg");
    let run = tmp.join("run");
    let bin = tmp.join("bin");
    let log = tmp.join("notify.log");
    for d in [&notes, &wt, &cfg, &run, &bin] {
        std::fs::create_dir_all(d).unwrap();
    }
    let write_stub = |name: &str, body: &str| {
        let p = bin.join(name);
        std::fs::write(&p, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    };
    write_stub("git", "#!/bin/sh\nexit 1\n");
    write_stub("gh", "#!/bin/sh\nexit 1\n");
    // Record each call as one CALL<TAB>argv… line so payload + count are assertable.
    write_stub(
        "notify-send",
        &format!(
            "#!/bin/sh\n{{ printf 'CALL'; for a in \"$@\"; do printf '\\t%s' \"$a\"; done; \
             printf '\\n'; }} >> \"{}\"\n",
            log.display()
        ),
    );
    std::fs::write(&log, "").unwrap();
    let path_env = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    JobFixture {
        notes,
        wt,
        cfg,
        run,
        log,
        path_env,
    }
}

impl JobFixture {
    /// A `bn <args>` run in the fixture: non-repo cwd, deterministic clock, isolated trees, stub bins.
    fn bn(&self, args: &[&str]) -> std::process::Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_bn"));
        c.current_dir(&self.run)
            .env("PATH", &self.path_env)
            .env("BN_NOTES_DIR", &self.notes)
            .env("BN_WORKTREE_DIR", &self.wt)
            .env("BN_HOSTNAME", "h")
            .env("BN_TODAY", "2026-06-26")
            .env("BN_CONFIG_DIR", &self.cfg)
            .args(args);
        c.output().unwrap()
    }
    /// Run a native toast job with its enable flag (`BN_STANDUP_ENABLED`/`BN_WRAP_ENABLED`).
    ///
    /// Runs with the notification bar wide open. Standup and wrap are `Digest`-class, so the
    /// shipped default bar (`blocking`) holds them at the policy gate — these validators are about
    /// the job's *payload*, so they open the bar to reach the backend. That the default bar keeps
    /// them silent is asserted separately, in `digest_jobs_do_not_toast_under_the_default_bar`.
    fn job(&self, enable_var: &str, enabled: &str, job: &str) -> String {
        self.job_with_bar(enable_var, enabled, job, "all")
    }

    /// [`Self::job`] with an explicit `[notify].bar` (via `$BN_NOTIFY_BAR`).
    fn job_with_bar(&self, enable_var: &str, enabled: &str, job: &str, bar: &str) -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_bn"))
            .current_dir(&self.run)
            .env("PATH", &self.path_env)
            .env("BN_NOTES_DIR", &self.notes)
            .env("BN_WORKTREE_DIR", &self.wt)
            .env("BN_HOSTNAME", "h")
            .env("BN_TODAY", "2026-06-26")
            .env("BN_CONFIG_DIR", &self.cfg)
            .env("BN_NOTIFY_BAR", bar)
            .env(enable_var, enabled)
            .args(["run", job])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
    }
    fn notify_calls(&self) -> usize {
        read(&self.log)
            .lines()
            .filter(|l| l.starts_with("CALL"))
            .count()
    }
    fn log(&self) -> String {
        read(&self.log)
    }
}

/// Ported from `validate-standup.sh`: `bn run standup-push` counts today's seeded plan into the
/// copy-paste line and toasts it once; two runs toast twice with a stable line (idempotent); and
/// with autopilot OFF it pushes nothing (no toast).
#[test]
fn standup_push_content_idempotent_and_silent_when_off() {
    // content + notify payload
    let t1 = tempfile::tempdir().unwrap();
    let f = job_fixture(t1.path());
    assert!(f
        .bn(&["daily", "add", "review the wrap PR"])
        .status
        .success());
    let out = f.job("BN_STANDUP_ENABLED", "1", "standup-push");
    assert!(
        out.contains("today 1 planned"),
        "copy line counts today's plan: {out}"
    );
    assert_eq!(f.notify_calls(), 1, "toasted exactly once");
    assert!(
        f.log().contains("bn standup"),
        "payload title is the standup header"
    );
    assert!(
        f.log()
            .contains("Standup: yesterday shipped 0, today 1 planned"),
        "payload body is the copy-paste line"
    );

    // idempotent
    let t2 = tempfile::tempdir().unwrap();
    let f = job_fixture(t2.path());
    assert!(f
        .bn(&["daily", "add", "review the wrap PR"])
        .status
        .success());
    let last = |s: &str| s.lines().last().unwrap_or_default().to_string();
    let first = last(&f.job("BN_STANDUP_ENABLED", "1", "standup-push"));
    let second = last(&f.job("BN_STANDUP_ENABLED", "1", "standup-push"));
    assert_eq!(second, first, "copy line stable across runs");
    // The copy line is idempotent, but the *interruption* is not repeated: both runs share the
    // delivery key `standup:<today>`, so the policy gate's cooldown holds the second toast.
    assert_eq!(
        f.notify_calls(),
        1,
        "two runs → one toast (the second is inside the key cooldown)"
    );

    // silent when off
    let t3 = tempfile::tempdir().unwrap();
    let f = job_fixture(t3.path());
    assert!(f
        .bn(&["daily", "add", "review the wrap PR"])
        .status
        .success());
    let out = f.job("BN_STANDUP_ENABLED", "0", "standup-push");
    assert!(
        out.contains("disabled — nothing to push"),
        "no-op message: {out}"
    );
    assert_eq!(f.notify_calls(), 0, "notify NOT invoked when off");
}

/// Ported from `validate-wrap.sh`: `bn run wrap-digest` names the seeded daily goal in a dated
/// digest and toasts once; two runs toast twice with stable output; a clean day emits no digest and
/// no toast; and with the digest disabled it pushes nothing.
#[test]
fn wrap_digest_content_idempotent_and_silent_when_clean_or_off() {
    // content + notify payload
    let t1 = tempfile::tempdir().unwrap();
    let f = job_fixture(t1.path());
    assert!(f
        .bn(&["daily", "add", "ship the validate scripts"])
        .status
        .success());
    let out = f.job("BN_WRAP_ENABLED", "1", "wrap-digest");
    assert!(
        out.contains("ship the validate scripts"),
        "digest names the carried goal: {out}"
    );
    assert!(
        out.contains("Wrap — 2026-06-26"),
        "digest is dated for today: {out}"
    );
    assert_eq!(f.notify_calls(), 1, "toasted exactly once");
    assert!(
        f.log().contains("bn wrap — end of day"),
        "payload title is the wrap header"
    );
    assert!(
        f.log().contains("ship the validate scripts"),
        "payload body carries the goal"
    );

    // idempotent
    let t2 = tempfile::tempdir().unwrap();
    let f = job_fixture(t2.path());
    assert!(f
        .bn(&["daily", "add", "ship the validate scripts"])
        .status
        .success());
    let first = f.job("BN_WRAP_ENABLED", "1", "wrap-digest");
    let second = f.job("BN_WRAP_ENABLED", "1", "wrap-digest");
    assert_eq!(second, first, "output stable across runs");
    // As with standup: stable output, but one interruption — both runs key `wrap:<today>`.
    assert_eq!(
        f.notify_calls(),
        1,
        "two runs → one toast (the second is inside the key cooldown)"
    );

    // silent when clean
    let t3 = tempfile::tempdir().unwrap();
    let f = job_fixture(t3.path());
    let out = f.job("BN_WRAP_ENABLED", "1", "wrap-digest");
    assert!(out.contains("clean day"), "reports a clean day: {out}");
    assert!(
        !out.contains("Carrying into tomorrow"),
        "no carry section on a clean day"
    );
    assert_eq!(f.notify_calls(), 0, "notify NOT invoked on a clean day");

    // silent when disabled
    let t4 = tempfile::tempdir().unwrap();
    let f = job_fixture(t4.path());
    assert!(f
        .bn(&["daily", "add", "ship the validate scripts"])
        .status
        .success());
    let out = f.job("BN_WRAP_ENABLED", "0", "wrap-digest");
    assert!(out.contains("nothing to push"), "no-op message: {out}");
    assert_eq!(f.notify_calls(), 0, "notify NOT invoked when disabled");
}

/// The shipped default: a toast means work has stopped and needs your decision, so the scheduled
/// digests produce their content and reach the policy gate but never interrupt. The two tests
/// above deliberately raise the bar to inspect the payload; this one pins the default.
#[test]
fn digest_jobs_do_not_toast_under_the_default_bar() {
    for (enable_var, job, marker) in [
        ("BN_STANDUP_ENABLED", "standup-push", "today 1 planned"),
        (
            "BN_WRAP_ENABLED",
            "wrap-digest",
            "ship the validate scripts",
        ),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let f = job_fixture(tmp.path());
        assert!(f
            .bn(&["daily", "add", "ship the validate scripts"])
            .status
            .success());
        let out = f.job_with_bar(enable_var, "1", job, "blocking");
        assert!(
            out.contains(marker),
            "{job} still produces its content: {out}"
        );
        assert_eq!(
            f.notify_calls(),
            0,
            "{job} is a digest — it must not interrupt under the default bar"
        );
    }
}

// ---------------------------------------------------------------------------
// Phase 3b batch 4: verify-gate / reap / trust-folder / afk / watch. Each drives
// the built `bn` in an isolated sandbox (bare repo, fake HOME, and — for the
// fleet/attention jobs — a stub `tmux` on PATH), asserting the same observable
// contract the shell harness checked, now with serde_json instead of jq.
// ---------------------------------------------------------------------------

/// Write `body` to `path` and make it executable (unix) — a self-contained stub/script.
fn write_exec(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// A `bn` command in a bare-repo sandbox: notes/wt roots, host `h`, `TMUX` cleared (so dispatch
/// stays headless), cwd at `cwd`.
fn sandbox_bn(notes: &Path, wt: &Path, cwd: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_bn"));
    c.env("BN_NOTES_DIR", notes)
        .env("BN_WORKTREE_DIR", wt)
        .env("BN_HOSTNAME", "h")
        .env_remove("TMUX")
        .current_dir(cwd);
    c
}

/// Parse a JSONC config (drop whole-line `//` comments) into a `serde_json::Value`. Mirrors the
/// copilot `config.json` reader the trust-folder validator used in python.
fn parse_jsonc(raw: &str) -> serde_json::Value {
    let body: String = raw
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::from_str(&body).expect("copilot config.json parses after stripping // lines")
}

fn cop_trusted(cfg: &serde_json::Value, path: &str) -> bool {
    cfg["trustedFolders"]
        .as_array()
        .map(|a| a.iter().any(|x| x.as_str() == Some(path)))
        .unwrap_or(false)
}

/// Ported from `validate-verify-gate.sh`: `bn dispatch --verify` records the gate in the note;
/// `bn verify` runs the branch-note `test` job and exits 0 / records PASS on success, exits
/// non-zero / records FAIL on failure.
#[test]
fn verify_gate_records_pass_fail_and_dispatch_gate() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();
    let (notes, wt, _bare) = bare_sandbox(t, "sandbox");
    let note = notes.join("h").join("sandbox").join("feat-v");
    let wtdir = wt.join("sandbox").join("feat-v");

    let out = sandbox_bn(&notes, &wt, t)
        .args([
            "--json",
            "dispatch",
            "--no-pool",
            "--repo",
            "sandbox",
            "--branch",
            "feat/v",
            "--no-tmux",
            "--verify",
            "add a thing",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "dispatch --verify failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v["data"]["verify"],
        serde_json::json!(true),
        "json verify=true"
    );
    assert!(
        read(&note.join("note.md"))
            .to_lowercase()
            .contains("gate: run"),
        "gate recorded in note"
    );

    let jobs = note.join("jobs");
    std::fs::create_dir_all(&jobs).unwrap();

    // Passing test job → exit 0, note records PASS.
    write_exec(
        &jobs.join("test.sh"),
        "#!/bin/sh\necho 'tests ok'\nexit 0\n",
    );
    let out = sandbox_bn(&notes, &wt, &wtdir)
        .args(["verify", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "verify pass exits 0");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["data"]["pass"], serde_json::json!(true), "json pass=true");
    assert!(
        read(&note.join("note.md"))
            .to_lowercase()
            .contains("verify test: pass"),
        "PASS recorded to note"
    );

    // Failing test job → non-zero, note records FAIL.
    write_exec(&jobs.join("test.sh"), "#!/bin/sh\necho boom >&2\nexit 1\n");
    let out = sandbox_bn(&notes, &wt, &wtdir)
        .args(["verify", "--json"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "verify fail is non-zero");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v["data"]["pass"],
        serde_json::json!(false),
        "json pass=false"
    );
    assert!(
        read(&note.join("note.md"))
            .to_lowercase()
            .contains("verify test: fail"),
        "FAIL recorded to note"
    );
}

/// Ported from `validate-reap.sh`: `bn wt reap` reaps a done+clean scout worktree (keeping its
/// note), and keeps a still-active scout and a done-but-dirty scout.
#[test]
fn reap_removes_done_clean_scout_keeps_active_and_dirty() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();
    let (notes, wt, _bare) = bare_sandbox(t, "sandbox");
    let wts = wt.join("sandbox");

    let dispatch = |branch: &str, text: &str| {
        sandbox_bn(&notes, &wt, t)
            .args([
                "dispatch",
                "--no-pool",
                "--repo",
                "sandbox",
                "--branch",
                branch,
                "--scout",
                "--no-tmux",
                text,
            ])
            .output()
            .unwrap()
    };
    let done = |branch: &str, summary: &str| {
        sandbox_bn(&notes, &wt, t)
            .args([
                "agent",
                "done",
                "--repo",
                "sandbox",
                "--branch",
                branch,
                "--summary",
                summary,
            ])
            .output()
            .unwrap()
    };

    dispatch("scout/done", "investigate A");
    done("scout-done", "found A");
    dispatch("scout/active", "investigate B");
    dispatch("scout/dirty", "investigate C");
    done("scout-dirty", "found C");
    std::fs::write(wts.join("scout-dirty").join("uncommitted.txt"), "x").unwrap();

    // #904 added a 20-min grace after `bn agent done` (the note's last write) before a done scout
    // is auto-reaped, so its tmux window doesn't vanish while the result is still being read. Age
    // the finished scout's note past that grace so this test exercises the reap *decision* (done +
    // clean → reap; active/dirty → keep), not the wall-clock timer #904 added around it.
    let done_note = notes
        .join("h")
        .join("sandbox")
        .join("scout-done")
        .join("note.md");
    let past_grace = std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 60);
    std::fs::File::options()
        .write(true)
        .open(&done_note)
        .and_then(|f| f.set_modified(past_grace))
        .unwrap();

    let dry = String::from_utf8_lossy(
        &sandbox_bn(&notes, &wt, t)
            .args(["wt", "reap", "--all"])
            .output()
            .unwrap()
            .stdout,
    )
    .into_owned();
    assert!(
        dry.contains("would reap scout-done"),
        "dry-run reaps scout-done: {dry}"
    );
    assert!(!dry.contains("would reap scout-active"), "keeps active");
    assert!(!dry.contains("would reap scout-dirty"), "keeps dirty");

    sandbox_bn(&notes, &wt, t)
        .args(["wt", "reap", "--all", "--yes"])
        .output()
        .unwrap();
    assert!(!wts.join("scout-done").is_dir(), "done scout removed");
    assert!(wts.join("scout-active").is_dir(), "active scout kept");
    assert!(wts.join("scout-dirty").is_dir(), "dirty scout kept");
    assert!(
        notes
            .join("h")
            .join("sandbox")
            .join("scout-done")
            .join("note.md")
            .is_file(),
        "findings note survives reap"
    );
}

/// Ported from `validate-trust-folder.sh`: `bn trust-folder` marks the worktree trusted for both
/// harnesses (claude `hasTrustDialogAccepted`, copilot `trustedFolders`), preserves the copilot
/// JSONC header and existing entries, is idempotent, and honours `--harness` scoping.
#[test]
fn trust_folder_marks_both_harnesses_idempotent_and_scoped() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();
    let home = t.join("home");
    std::fs::create_dir_all(home.join(".copilot")).unwrap();
    let wtdir = t.join("wt").join("repo").join("feat");
    std::fs::create_dir_all(&wtdir).unwrap();
    std::fs::write(home.join(".claude.json"), r#"{"projects":{}}"#).unwrap();
    std::fs::write(
        home.join(".copilot").join("config.json"),
        "// managed automatically.\n{\n  \"trustedFolders\": [\"/existing\"]\n}\n",
    )
    .unwrap();
    let rp = std::fs::canonicalize(&wtdir).unwrap();
    let rp = rp.to_str().unwrap();

    let trust = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_bn"))
            .env("HOME", &home)
            .args(args)
            .output()
            .unwrap()
    };

    trust(&["trust-folder", wtdir.to_str().unwrap()]);
    let claude: serde_json::Value =
        serde_json::from_str(&read(&home.join(".claude.json"))).unwrap();
    assert_eq!(
        claude["projects"][rp]["hasTrustDialogAccepted"],
        serde_json::json!(true),
        "claude worktree trusted"
    );
    let cop_raw = read(&home.join(".copilot").join("config.json"));
    assert!(
        cop_raw
            .lines()
            .next()
            .unwrap()
            .contains("managed automatically"),
        "copilot header preserved"
    );
    let cop = parse_jsonc(&cop_raw);
    assert!(cop_trusted(&cop, rp), "copilot worktree trusted");
    assert!(
        cop_trusted(&cop, "/existing"),
        "copilot existing entry kept"
    );

    // Idempotent: re-run adds no duplicate.
    trust(&["trust-folder", wtdir.to_str().unwrap()]);
    let cop = parse_jsonc(&read(&home.join(".copilot").join("config.json")));
    let dupes = cop["trustedFolders"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x.as_str() == Some(rp))
        .count();
    assert_eq!(dupes, 1, "no duplicate copilot entry on re-run");

    // --harness claude scopes to claude only.
    let wt2 = t.join("wt").join("repo").join("feat2");
    std::fs::create_dir_all(&wt2).unwrap();
    let rp2 = std::fs::canonicalize(&wt2).unwrap();
    let rp2 = rp2.to_str().unwrap();
    trust(&["trust-folder", wt2.to_str().unwrap(), "--harness", "claude"]);
    let claude: serde_json::Value =
        serde_json::from_str(&read(&home.join(".claude.json"))).unwrap();
    assert_eq!(
        claude["projects"][rp2]["hasTrustDialogAccepted"],
        serde_json::json!(true),
        "claude-only: claude set"
    );
    let cop = parse_jsonc(&read(&home.join(".copilot").join("config.json")));
    assert!(!cop_trusted(&cop, rp2), "claude-only: copilot NOT set");
}

/// Ported from `validate-afk.sh`: `bn afk end` escalates agents that need you (a blocked window)
/// plus urgent attention, and absorbs the routine (a working window, an info item). A stub `tmux`
/// supplies the window states; an isolated HOME/notes keeps the read hermetic.
#[test]
fn afk_end_escalates_blocked_and_urgent_absorbs_routine() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();
    let home = t.join("home");
    let notes = t.join("notes");
    let bin = t.join("bin");
    for d in [&home, &notes, &bin] {
        std::fs::create_dir_all(d).unwrap();
    }
    write_exec(
        &bin.join("tmux"),
        "#!/bin/sh\nfor a in \"$@\"; do [ \"$a\" = \"list-windows\" ] && LW=1; done\n\
         [ -n \"${LW:-}\" ] && { printf 'sandbox\\tblk\\tblocked\\t0\\n'; \
         printf 'sandbox\\twrk\\tworking\\t0\\n'; }\nexit 0\n",
    );
    let path_env = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let bn = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_bn"))
            .env("HOME", &home)
            .env("BN_NOTES_DIR", &notes)
            .env("BN_HOSTNAME", "h")
            .env("PATH", &path_env)
            .args(args)
            .output()
            .unwrap()
    };

    bn(&[
        "attn",
        "add",
        "prod is down",
        "--severity",
        "urgent",
        "--source",
        "test",
    ]);
    bn(&[
        "attn",
        "add",
        "fyi minor note",
        "--severity",
        "info",
        "--source",
        "test",
    ]);
    bn(&["afk", "start"]);
    assert!(
        String::from_utf8_lossy(&bn(&["afk"]).stdout)
            .to_lowercase()
            .contains("away"),
        "afk status shows away"
    );

    let out = bn(&["--json", "afk", "end"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["data"]["away"], serde_json::json!(false), "away cleared");
    let agents = v["data"]["agents"].as_array().cloned().unwrap_or_default();
    assert!(
        agents.iter().any(|a| a["target"] == "sandbox:blk"),
        "blocked agent escalated"
    );
    assert!(
        !agents.iter().any(|a| a["target"] == "sandbox:wrk"),
        "working agent absorbed"
    );
    let attn = v["data"]["attention"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        attn.iter().any(|x| x["severity"] == "urgent"),
        "urgent attention listed"
    );
    assert!(
        !attn.iter().any(|x| x["severity"] == "info"),
        "info attention absorbed"
    );
    assert!(
        String::from_utf8_lossy(&bn(&["afk"]).stdout)
            .to_lowercase()
            .contains("not away"),
        "afk status now not away"
    );
}

/// Ported from `validate-watch.sh`: `bn watch --once` reports NEW wake events, dedups them against
/// the seen-set on the next pass, wakes again for a freshly-added urgent item, and labels a
/// staff-sourced item `staff-<agent>: <title>` (#265). Stub `tmux` + isolated HOME/notes/state.
#[test]
fn watch_dedups_wake_events_and_labels_staff_source() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();
    let home = t.join("home");
    let notes = t.join("notes");
    let state = t.join("state");
    let bin = t.join("bin");
    for d in [&home, &notes, &state, &bin] {
        std::fs::create_dir_all(d).unwrap();
    }
    write_exec(&bin.join("tmux"), "#!/bin/sh\nexit 0\n");
    let path_env = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let bn = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_bn"))
            .env("HOME", &home)
            .env("BN_NOTES_DIR", &notes)
            .env("BN_STATE_DIR", &state)
            .env("BN_HOSTNAME", "h")
            .env("PATH", &path_env)
            .args(args)
            .output()
            .unwrap()
    };
    let new_count = || {
        let out = bn(&["--json", "watch", "--once"]);
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        v["data"]["new"].as_array().map(|a| a.len()).unwrap_or(0)
    };

    bn(&[
        "add", "todo", "seed", "--repo", "widget", "--branch", "feat",
    ]);
    bn(&[
        "agent",
        "done",
        "--repo",
        "widget",
        "--branch",
        "feat",
        "--summary",
        "shipped",
    ]);
    bn(&[
        "attn",
        "add",
        "prod is down",
        "--severity",
        "urgent",
        "--source",
        "test",
    ]);

    assert_eq!(new_count(), 2, "pass 1: two new wake events");
    assert_eq!(new_count(), 0, "pass 2: deduped");

    bn(&[
        "attn",
        "add",
        "second alert",
        "--severity",
        "urgent",
        "--source",
        "test",
    ]);
    assert_eq!(new_count(), 1, "pass 3: fresh urgent wakes");
    assert_eq!(new_count(), 0, "pass 4: quiet again");

    bn(&[
        "attn",
        "add",
        "dependency CVE in serde",
        "--severity",
        "urgent",
        "--source",
        "staff-sentinel",
    ]);
    let out = bn(&["--json", "watch", "--once"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v["data"]["new"][0]["label"].as_str(),
        Some("staff-sentinel: dependency CVE in serde"),
        "staff toast carries the agent name"
    );
}

// ---------------------------------------------------------------------------
// Phase 3b batch 5: the remaining CLI-sandbox validators. Fleet/live/supervise
// read fake harness session dirs under a sandbox HOME (+ a stub `tmux` on PATH
// for Plane-A rows); the rest use the bare-repo sandbox. `date -u` supplies a
// live timestamp so recency-based health classifies as active.
// ---------------------------------------------------------------------------

/// A live UTC RFC3339 timestamp (`date -u +%Y-%m-%dT%H:%M:%SZ`), so fake sessions read as "now".
fn now_utc() -> String {
    let out = Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Ported from `validate-agent-done.sh`: `bn agent done` records a keyed completion in the review
/// inbox (source=agent, repo/branch/PR), keeps it OUT of the actionable attention board, records
/// it in the note Progress, dedups on the key, updates the item when a later done attaches a PR,
/// and `bn attn ack` folds it out of the inbox.
#[test]
fn agent_done_records_completion_dedups_updates_and_ack_clears() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();
    let notes = t.join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    let note = notes.join("h").join("sandbox").join("feat").join("note.md");
    let pr = "https://github.com/o/r/pull/42";
    let bn = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_bn"))
            .env("BN_NOTES_DIR", &notes)
            .env("BN_HOSTNAME", "h")
            .env_remove("TMUX")
            .args(args)
            .output()
            .unwrap()
    };
    let feed = || -> serde_json::Value {
        serde_json::from_slice(&bn(&["--json", "feed"]).stdout).unwrap()
    };
    let completed_count = |k: &str| {
        feed()["data"]["completed"]
            .as_array()
            .map(|a| a.iter().filter(|i| i["key"] == k).count())
            .unwrap_or(0)
    };

    bn(&[
        "add", "todo", "seed", "--repo", "sandbox", "--branch", "feat",
    ]);
    bn(&[
        "agent",
        "done",
        "--repo",
        "sandbox",
        "--branch",
        "feat",
        "--summary",
        "added the greeting function",
        "--pr",
        pr,
    ]);

    let f = feed();
    let item = f["data"]["completed"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["key"] == "done:sandbox/feat")
        .cloned()
        .expect("completion in review inbox");
    assert_eq!(item["source"], "agent", "source is agent");
    assert_eq!(item["repo"], "sandbox");
    assert_eq!(item["branch"], "feat");
    assert_eq!(item["link"], pr, "PR link carried");
    assert!(item["title"]
        .as_str()
        .unwrap()
        .contains("added the greeting function"));

    let attn: serde_json::Value = serde_json::from_slice(&bn(&["--json", "attn"]).stdout).unwrap();
    let in_board = ["to_start", "needs_you", "in_progress"].iter().any(|k| {
        attn["data"][k]
            .as_array()
            .map(|a| a.iter().any(|i| i["key"] == "done:sandbox/feat"))
            .unwrap_or(false)
    });
    assert!(!in_board, "completion NOT on the attention board");
    assert!(
        read(&note).contains("agent done: added the greeting function"),
        "recorded in note Progress"
    );

    // Idempotent: a second done dedups on the key.
    bn(&[
        "agent",
        "done",
        "--repo",
        "sandbox",
        "--branch",
        "feat",
        "--summary",
        "again",
    ]);
    assert_eq!(completed_count("done:sandbox/feat"), 1, "no duplicate");

    // scout→ship: a later done with a PR UPDATES the item, still one.
    bn(&[
        "add", "todo", "seed", "--repo", "sandbox", "--branch", "ship",
    ]);
    bn(&[
        "agent",
        "done",
        "--repo",
        "sandbox",
        "--branch",
        "ship",
        "--summary",
        "scouted",
    ]);
    let pr2 = "https://github.com/o/r/pull/77";
    let out: serde_json::Value = serde_json::from_slice(
        &bn(&[
            "--json",
            "agent",
            "done",
            "--repo",
            "sandbox",
            "--branch",
            "ship",
            "--summary",
            "shipped",
            "--pr",
            pr2,
        ])
        .stdout,
    )
    .unwrap();
    assert_eq!(
        out["data"]["updated"],
        serde_json::json!(true),
        "second done updates"
    );
    let ship = feed()["data"]["completed"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["key"] == "done:sandbox/ship")
        .cloned()
        .unwrap();
    assert_eq!(ship["link"], pr2, "PR now attached");
    assert_eq!(completed_count("done:sandbox/ship"), 1, "still one item");

    // ack folds it out of the inbox.
    let id = item["id"].as_str().unwrap();
    bn(&["attn", "ack", id]);
    assert_eq!(
        completed_count("done:sandbox/feat"),
        0,
        "ack clears from inbox"
    );
}

/// Ported from `validate-fleet-copilot.sh`: `bn agent supervise` enumerates BOTH a live Claude and
/// a live Copilot session (tagged by harness), and excludes a Copilot session that carries a
/// shutdown record. Sandbox HOME → it reads the fake `~/.claude` + `~/.copilot` dirs.
#[test]
fn fleet_copilot_enumerates_both_harnesses_and_excludes_finished() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();
    let home = t.join("home");
    let wt = t.join("wt");
    let notes = t.join("notes");
    let cl = home.join(".claude").join("projects").join("enc");
    let cop = home.join(".copilot").join("session-state");
    for d in [&cl, &cop, &wt, &notes] {
        std::fs::create_dir_all(d).unwrap();
    }
    let now = now_utc();
    let cwd_cl = wt.join("sandbox").join("cl");
    std::fs::write(
        cl.join("aaaa.jsonl"),
        format!(
            "{{\"type\":\"user\",\"sessionId\":\"aaaa\",\"gitBranch\":\"cl\",\"cwd\":\"{cwd}\",\"timestamp\":\"{now}\"}}\n\
             {{\"type\":\"assistant\",\"sessionId\":\"aaaa\",\"gitBranch\":\"cl\",\"cwd\":\"{cwd}\",\"timestamp\":\"{now}\",\"message\":{{\"content\":[{{\"type\":\"tool_use\"}}]}}}}\n",
            cwd = cwd_cl.display()
        ),
    )
    .unwrap();

    let mk_copilot = |sid: &str, br: &str, shutdown: bool| {
        let d = cop.join(sid);
        std::fs::create_dir_all(&d).unwrap();
        let cwd = wt.join("sandbox").join(br);
        let mut body = format!(
            "{{\"type\":\"session.start\",\"timestamp\":\"{now}\",\"data\":{{\"context\":{{\"cwd\":\"{}\",\"branch\":\"{br}\"}}}}}}\n\
             {{\"type\":\"assistant.turn_start\",\"timestamp\":\"{now}\"}}\n\
             {{\"type\":\"tool.execution_start\",\"timestamp\":\"{now}\"}}\n",
            cwd.display()
        );
        if shutdown {
            body.push_str(&format!(
                "{{\"type\":\"session.shutdown\",\"timestamp\":\"{now}\",\"data\":{{}}}}\n"
            ));
        }
        std::fs::write(d.join("events.jsonl"), body).unwrap();
        std::fs::write(
            d.join("workspace.yaml"),
            format!("id: {sid}\ncwd: {}\nbranch: {br}\n", cwd.display()),
        )
        .unwrap();
    };
    mk_copilot("bbbb", "cop", false);
    mk_copilot("cccc", "done", true);

    let out = Command::new(env!("CARGO_BIN_EXE_bn"))
        .env("HOME", &home)
        .env("BN_WORKTREE_DIR", &wt)
        .env("BN_NOTES_DIR", &notes)
        .env("BN_HOSTNAME", "h")
        .args(["--json", "agent", "supervise"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let agents = v["data"]["agents"].as_array().cloned().unwrap_or_default();
    assert_eq!(agents.len(), 2, "exactly 2 live agents");
    let mut harnesses: Vec<&str> = agents
        .iter()
        .filter_map(|a| a["harness"].as_str())
        .collect();
    harnesses.sort_unstable();
    harnesses.dedup();
    assert_eq!(
        harnesses,
        vec!["claude", "copilot"],
        "both harnesses present"
    );
    assert!(
        agents
            .iter()
            .any(|a| a["harness"] == "copilot" && a["target"] == "sandbox:cop"),
        "live copilot listed"
    );
    assert!(
        !agents.iter().any(|a| a["target"] == "sandbox:done"),
        "finished copilot excluded"
    );
}

/// Ported from `validate-agent-live.sh`: `bn agent live` joins Plane A (tmux window decoration) and
/// Plane B (session-log health) into one row per agent — a case-mismatched session name still joins
/// to one row, an undecorated shell window is dropped, and a fleet agent whose window is gone is
/// kept with `has_window=false`. `bn fleet` renders the same fixtures.
#[test]
fn agent_live_joins_planes_and_fleet_board_renders() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();
    let home = t.join("home");
    let wt = t.join("wt");
    let notes = t.join("notes");
    let bin = t.join("bin");
    for d in [&home, &wt, &notes, &bin] {
        std::fs::create_dir_all(d).unwrap();
    }
    write_exec(
        &bin.join("tmux"),
        "#!/bin/sh\nsub=\"$1\"\ncase \"$sub\" in\n  list-windows)\n    \
         printf 'Arena\\tfeat-x\\twaiting\\t0\\n'\n    printf 'sandbox\\tshell\\t\\t0\\n'\n    ;;\n  \
         capture-pane)\n    printf 'Running the build...\\nWaiting for your confirmation to proceed\\n'\n    ;;\n\
         esac\nexit 0\n",
    );
    let path_env = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let now = now_utc();
    let mk_copilot = |sid: &str, repo: &str, br: &str| {
        let d = home.join(".copilot").join("session-state").join(sid);
        std::fs::create_dir_all(&d).unwrap();
        let cwd = wt.join(repo).join(br);
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(
            d.join("events.jsonl"),
            format!(
                "{{\"type\":\"session.start\",\"timestamp\":\"{now}\",\"data\":{{\"context\":{{\"cwd\":\"{}\",\"branch\":\"{br}\"}}}}}}\n\
                 {{\"type\":\"assistant.turn_start\",\"timestamp\":\"{now}\"}}\n",
                cwd.display()
            ),
        )
        .unwrap();
        std::fs::write(
            d.join("workspace.yaml"),
            format!("id: {sid}\ncwd: {}\nbranch: {br}\n", cwd.display()),
        )
        .unwrap();
    };
    mk_copilot("s1", "arena", "feat-x");
    mk_copilot("s2", "bn", "gone");

    let bn_live = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_bn"))
            .env("HOME", &home)
            .env("BN_WORKTREE_DIR", &wt)
            .env("BN_NOTES_DIR", &notes)
            .env("BN_HOSTNAME", "h")
            .env("PATH", &path_env)
            .args(args)
            .output()
            .unwrap()
    };

    let v: serde_json::Value =
        serde_json::from_slice(&bn_live(&["--json", "agent", "live"]).stdout).unwrap();
    let agents = v["data"]["agents"].as_array().cloned().unwrap_or_default();
    assert_eq!(agents.len(), 2, "two joined rows (shell dropped)");
    assert!(
        agents.iter().any(|a| a["target"] == "arena:feat-x"
            && a["state"] == "waiting"
            && a["health"] == "active"
            && a["has_window"] == serde_json::json!(true)
            && a["harness"] == "copilot"),
        "case-mismatch joined into one row: {agents:?}"
    );
    assert!(
        agents
            .iter()
            .any(|a| a["target"] == "bn:gone" && a["has_window"] == serde_json::json!(false)),
        "windowless fleet agent kept"
    );
    assert!(
        !agents.iter().any(|a| a["target"] == "sandbox:shell"),
        "no plain-shell row leaked"
    );

    let board = String::from_utf8_lossy(&bn_live(&["fleet", "--preview"]).stdout).into_owned();
    assert!(board.contains("Fleet"), "board header renders");
    assert!(board.contains("arena:feat-x"), "waiting agent shown");
    assert!(board.contains("needs you"), "needs-you rank for waiting");
    assert!(board.contains("bn:gone"), "windowless agent shown");
    assert!(
        board.contains("Waiting for your confirmation"),
        "live pane preview included"
    );
}

/// First non-empty line under the note's `## Goal` heading (mirrors the shell `goal_of` awk).
fn goal_of(note_md: &Path) -> String {
    let body = read(note_md);
    let mut in_goal = false;
    for line in body.lines() {
        if line.starts_with("## Goal") {
            in_goal = true;
            continue;
        }
        if in_goal && !line.trim().is_empty() {
            return line.to_string();
        }
    }
    String::new()
}

/// Ported from `validate-dispatch-anywhere.sh`: `bn dispatch` spins up isolated work on ANY repo
/// from a non-repo cwd — worktree off the right base, seeded note (goal + source), agent-kind
/// normalization, strict per-dispatch isolation, scout/ship/yolo classification, promote (incl.
/// the report-only refusal), idempotency, recorded session id, and the guards.
#[test]
fn dispatch_anywhere_core_isolation_promote_and_guards() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();
    let (notes, wt, bare) = bare_sandbox(t, "sandbox");
    // Add a second base branch `release` to the bare repo.
    git_in(
        &bare,
        &[
            "--git-dir",
            bare.to_str().unwrap(),
            "branch",
            "release",
            "main",
        ],
    );
    let wts = wt.join("sandbox");
    let ns = notes.join("h").join("sandbox");
    let json = |out: &std::process::Output| -> serde_json::Value {
        serde_json::from_slice(&out.stdout).unwrap()
    };
    // Pooling is the default dispatch path now, but these cases assert the *cold* worktree layout
    // (conventional per-branch dirs + promote/guard/reap semantics), so force `--no-pool` right
    // after the `dispatch` subcommand. The pooled default (and that notes still save on a leased
    // slot) is covered by `dispatch_pools_by_default_and_preserves_note`.
    let d = |args: &[&str]| {
        let mut a: Vec<&str> = Vec::with_capacity(args.len() + 1);
        for &arg in args {
            a.push(arg);
            if arg == "dispatch" {
                a.push("--no-pool");
            }
        }
        sandbox_bn(&notes, &wt, t).args(&a).output().unwrap()
    };

    // 1: core flow — --repo from a non-repo cwd.
    let v = json(&d(&[
        "--json",
        "dispatch",
        "--repo",
        "sandbox",
        "--no-tmux",
        "Add search filter",
    ]));
    assert_eq!(v["ok"], serde_json::json!(true));
    assert_eq!(v["data"]["repo"], "sandbox");
    assert_eq!(v["data"]["created"], serde_json::json!(true));
    assert_eq!(v["data"]["base"], "main");
    assert_eq!(v["data"]["branch"], "dispatch/add-search-filter");
    assert!(wts.join("dispatch-add-search-filter").is_dir());
    assert!(
        goal_of(&ns.join("dispatch-add-search-filter").join("note.md"))
            .to_lowercase()
            .contains("add search filter")
    );
    assert_eq!(v["data"]["tmux_opened"], serde_json::json!(false));

    // 2: --base forks from the named base.
    let v = json(&d(&[
        "--json",
        "dispatch",
        "--repo",
        "sandbox",
        "--base",
        "release",
        "--branch",
        "feat/from-release",
        "--no-tmux",
        "off release",
    ]));
    assert_eq!(v["data"]["base"], "release");
    assert!(wts.join("feat-from-release").is_dir());
    let br_sha = String::from_utf8_lossy(
        &Command::new("git")
            .args([
                "--git-dir",
                bare.to_str().unwrap(),
                "rev-parse",
                "feat/from-release",
            ])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    let rel_sha = String::from_utf8_lossy(
        &Command::new("git")
            .args(["--git-dir", bare.to_str().unwrap(), "rev-parse", "release"])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    assert_eq!(br_sha, rel_sha, "new branch is at release");

    // 3: URL item — source recorded.
    let v = json(&d(&[
        "--json",
        "dispatch",
        "--repo",
        "sandbox",
        "--branch",
        "feat/urly",
        "--no-tmux",
        "https://example.com/o/r/issues/42",
    ]));
    assert_eq!(v["data"]["url"], "https://example.com/o/r/issues/42");
    assert!(read(&ns.join("feat-urly").join("note.md"))
        .contains("source: https://example.com/o/r/issues/42"));

    // 4: --agent normalization.
    let a_kind = |kind: &str, slug: &str| -> String {
        json(&d(&[
            "--json",
            "dispatch",
            "--repo",
            "sandbox",
            "--branch",
            &format!("z/{slug}"),
            "--no-tmux",
            "--agent",
            kind,
            "x",
        ]))["data"]["agent"]
            .as_str()
            .unwrap_or("")
            .to_string()
    };
    assert_eq!(a_kind("copilot", "c1"), "copilot");
    assert_eq!(a_kind("claude", "a1"), "claude");
    assert_eq!(a_kind("claudecode", "a2"), "claude");
    assert_eq!(a_kind("cc", "a3"), "claude");

    // 5: isolation — two dispatches never cross-contaminate.
    d(&[
        "dispatch",
        "--repo",
        "sandbox",
        "--branch",
        "iso/one",
        "--no-tmux",
        "goal ONE only",
    ]);
    d(&[
        "dispatch",
        "--repo",
        "sandbox",
        "--branch",
        "iso/two",
        "--no-tmux",
        "goal TWO only",
    ]);
    assert!(wts.join("iso-one").is_dir() && wts.join("iso-two").is_dir());
    let g1 = goal_of(&ns.join("iso-one").join("note.md"));
    let g2 = goal_of(&ns.join("iso-two").join("note.md"));
    assert!(g1.contains("goal ONE only") && !g1.contains("TWO"));
    assert!(g2.contains("goal TWO only") && !g2.contains("ONE"));

    // 5a: scout vs ship + yolo gating.
    let v = json(&d(&[
        "--json",
        "dispatch",
        "--repo",
        "sandbox",
        "--branch",
        "look/one",
        "--no-tmux",
        "--scout",
        "investigate the flaky test",
    ]));
    assert_eq!(v["data"]["task"], "scout");
    assert!(read(&ns.join("look-one").join("note.md"))
        .to_lowercase()
        .contains("task: scout"));
    let v = json(&d(&[
        "--json",
        "dispatch",
        "--repo",
        "sandbox",
        "--branch",
        "build/one",
        "--no-tmux",
        "add a feature",
    ]));
    assert_eq!(v["data"]["task"], "ship");
    assert!(read(&ns.join("build-one").join("note.md"))
        .to_lowercase()
        .contains("task: ship"));
    assert_eq!(v["data"]["yolo"], serde_json::json!(false));
    let v = json(&d(&[
        "--json",
        "dispatch",
        "--repo",
        "sandbox",
        "--branch",
        "yolo/one",
        "--no-tmux",
        "--yolo",
        "trusted work",
    ]));
    assert_eq!(v["data"]["yolo"], serde_json::json!(true));
    let v = json(&d(&[
        "--json",
        "dispatch",
        "--repo",
        "sandbox",
        "--branch",
        "yolo/ext",
        "--no-tmux",
        "--yolo",
        "https://example.com/o/r/issues/9",
    ]));
    assert_eq!(
        v["data"]["yolo"],
        serde_json::json!(false),
        "external-url seed downgraded"
    );
    assert_eq!(v["data"]["yolo_blocked_external"], serde_json::json!(true));

    // promote flips scout → ship; report-only is refused.
    let v = json(&d(&[
        "--json", "promote", "--repo", "sandbox", "--branch", "look/one",
    ]));
    assert_eq!(v["ok"], serde_json::json!(true));
    assert!(read(&ns.join("look-one").join("note.md"))
        .to_lowercase()
        .contains("promoted from scout"));
    d(&[
        "dispatch",
        "--repo",
        "sandbox",
        "--branch",
        "look/report",
        "--scout",
        "--no-tmux",
        "investigate a recommendation",
    ]);
    d(&[
        "agent",
        "done",
        "--repo",
        "sandbox",
        "--branch",
        "look-report",
        "--report-only",
        "--summary",
        "just a recommendation",
    ]);
    let out = d(&[
        "--json",
        "promote",
        "--repo",
        "sandbox",
        "--branch",
        "look/report",
    ]);
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .to_lowercase()
            .contains("report-only")
            || String::from_utf8_lossy(&out.stderr)
                .to_lowercase()
                .contains("report-only")
    );
    assert!(!read(&ns.join("look-report").join("note.md"))
        .to_lowercase()
        .contains("promoted from scout"));

    // 5b: safety — re-dispatch refuses to share the existing worktree and preserves its note.
    d(&[
        "dispatch",
        "--repo",
        "sandbox",
        "--branch",
        "idem/one",
        "--no-tmux",
        "original goal",
    ]);
    let v = json(&d(&[
        "--json",
        "dispatch",
        "--repo",
        "sandbox",
        "--branch",
        "idem/one",
        "--no-tmux",
        "CLOBBER attempt",
    ]));
    assert_eq!(v["ok"], serde_json::json!(false));
    assert_eq!(v["error"]["code"], serde_json::json!("worktree-occupied"));
    let g = goal_of(&ns.join("idem-one").join("note.md"));
    assert!(g.contains("original goal") && !g.to_uppercase().contains("CLOBBER"));

    // 5c: session id recorded (stub tmux so the agent "launches").
    let bin = t.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_exec(&bin.join("tmux"), "#!/bin/sh\nexit 0\n");
    let path_env = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    sandbox_bn(&notes, &wt, t)
        .env("PATH", &path_env)
        .args([
            "dispatch",
            "--no-pool",
            "--repo",
            "sandbox",
            "--branch",
            "sess/one",
            "--agent",
            "copilot",
            "steerable work",
        ])
        .output()
        .unwrap();
    let sess_note = read(&ns.join("sess-one").join("note.md"));
    assert!(
        sess_note
            .lines()
            .any(|l| l.contains("agent-session: copilot ")
                && l.chars().filter(|c| *c == '-').count() >= 4),
        "session id recorded: {sess_note}"
    );

    // 6: guards.
    let out = d(&["dispatch", "--repo", "nope", "--no-tmux", "x"]);
    assert!(!out.status.success(), "bogus repo fails");
    let err = String::from_utf8_lossy(&out.stderr).to_lowercase();
    assert!(
        err.contains("unknown repo") && err.contains("sandbox"),
        "lists known repos: {err}"
    );
    let out = d(&["dispatch", "--no-tmux", "x"]);
    assert!(!out.status.success(), "no --repo outside a repo fails");
    assert!(String::from_utf8_lossy(&out.stderr).contains("--repo"));
    let out = d(&[
        "dispatch",
        "--repo",
        "sandbox",
        "--no-tmux",
        "--agent",
        "gemini",
        "x",
    ]);
    assert!(!out.status.success(), "bogus --agent fails");
    assert!(
        !wts.join("dispatch-x").is_dir(),
        "no worktree created on bad agent"
    );
}

/// Pool-by-default (`bn dispatch` leases a warm `bn wt pool` slot unless `--no-pool`): a plain
/// dispatch into a repo with a bn-managed bare clone lands in a `_pool/<n>` slot (not the
/// conventional cold worktree dir) AND still seeds its branch note there — the user-facing
/// guarantee that notes survive leased worktrees. `--no-pool` restores the cold layout.
#[test]
fn dispatch_pools_by_default_and_preserves_note() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();
    let (notes, wt, _bare) = bare_sandbox(t, "sandbox");
    let wts = wt.join("sandbox");
    let ns = notes.join("h").join("sandbox");
    let json = |out: &std::process::Output| -> serde_json::Value {
        serde_json::from_slice(&out.stdout).unwrap()
    };

    // Plain dispatch → pooled by default: leases a slot under `_pool/`, not the per-branch dir.
    let v = json(
        &sandbox_bn(&notes, &wt, t)
            .args([
                "--json",
                "dispatch",
                "--repo",
                "sandbox",
                "--branch",
                "feat/pooled",
                "--no-tmux",
                "wire up pooled goal",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(v["ok"], serde_json::json!(true));
    assert_eq!(
        v["data"]["pooled"],
        serde_json::json!(true),
        "dispatch pools by default"
    );
    assert_eq!(v["data"]["created"], serde_json::json!(true));
    assert_eq!(v["data"]["branch"], "feat/pooled");
    let worktree = v["data"]["worktree"].as_str().unwrap();
    assert!(
        std::path::Path::new(worktree).starts_with(wts.join("_pool")),
        "leased a pool slot, got {worktree}"
    );
    assert!(
        !wts.join("feat-pooled").is_dir(),
        "no cold per-branch worktree when pooled"
    );
    // The whole point: the branch note is still seeded on the leased slot.
    assert!(
        goal_of(&ns.join("feat-pooled").join("note.md"))
            .to_lowercase()
            .contains("wire up pooled goal"),
        "note seeded + goal saved on the leased slot"
    );

    // `--no-pool` opts out → the legacy cold worktree at the conventional per-branch dir.
    let v = json(
        &sandbox_bn(&notes, &wt, t)
            .args([
                "--json",
                "dispatch",
                "--no-pool",
                "--repo",
                "sandbox",
                "--branch",
                "feat/cold",
                "--no-tmux",
                "cold goal",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(v["ok"], serde_json::json!(true));
    assert_eq!(
        v["data"]["pooled"],
        serde_json::json!(false),
        "--no-pool cuts a cold worktree"
    );
    assert!(
        wts.join("feat-cold").is_dir(),
        "cold worktree at the conventional per-branch dir"
    );
    assert!(goal_of(&ns.join("feat-cold").join("note.md"))
        .to_lowercase()
        .contains("cold goal"));
}

/// Ported from `validate-async-steer.sh`: `bn agent send --detach` fires the resume turn detached
/// and returns immediately (doesn't block on a slow turn), reporting `via=resume-detached` + a log
/// path, and the detached turn actually starts. A slow stub `copilot` on PATH stands in for the
/// harness. (The oracle's timing-based "sync send blocks" contrast is intentionally dropped —
/// inherently flaky; the detach-returns-fast guard is the real regression check.)
#[test]
fn async_steer_detach_returns_fast_and_spawns_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();
    let notes = t.join("notes");
    let wt = t.join("wt");
    let bin = t.join("bin");
    for d in [&notes, &wt.join("widget").join("feat"), &bin] {
        std::fs::create_dir_all(d).unwrap();
    }
    let marker = t.join("turn-ran");
    write_exec(
        &bin.join("copilot"),
        &format!(
            "#!/bin/sh\necho started >> \"{m}\"\nsleep 8\necho done >> \"{m}\"\n",
            m = marker.display()
        ),
    );
    let path_env = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let bn = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_bn"))
            .env("BN_NOTES_DIR", &notes)
            .env("BN_WORKTREE_DIR", &wt)
            .env("BN_HOSTNAME", "h")
            .env("PATH", &path_env)
            .env_remove("TMUX")
            .args(args)
            .output()
            .unwrap()
    };
    bn(&[
        "add", "todo", "seed", "--repo", "widget", "--branch", "feat",
    ]);
    bn(&[
        "add",
        "collab",
        "agent-session: copilot 11111111-2222-3333-4444-555555555555",
        "--repo",
        "widget",
        "--branch",
        "feat",
    ]);

    let start = std::time::Instant::now();
    let out = bn(&[
        "--json",
        "agent",
        "send",
        "widget:feat",
        "do a long task",
        "--detach",
    ]);
    let elapsed = start.elapsed().as_secs();
    assert!(elapsed < 5, "detach returns fast (<5s), took {elapsed}s");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["data"]["via"], "resume-detached");
    let log = v["data"]["log"].as_str().unwrap_or("");
    assert!(!log.is_empty() && log != "null", "reports a log path");
    std::thread::sleep(std::time::Duration::from_secs(2));
    assert!(
        read(&marker).contains("started"),
        "the detached turn actually started"
    );
    assert!(Path::new(log).is_file(), "log file was created");
}

/// Ported from `validate-diff-kill-clean.sh`: `bn diff` renders sections from a real repo a commit
/// ahead (+ the `d` alias); `bn kill <port>` frees a listening port and is idempotent / rejects a
/// non-numeric port; `bn clean` removes empty-text frontmatter placeholders (idempotent, --json).
#[test]
fn diff_kill_clean_behaviors() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();

    // --- diff: origin + a worktree-style clone one commit ahead --------------
    let notes = t.join("notes");
    let wt = t.join("wt");
    let seed = t.join("seed");
    let origin = t.join("origin.git");
    std::fs::create_dir_all(&seed).unwrap();
    git_in(&seed, &["init", "-q"]);
    git_in(&seed, &["checkout", "-q", "-b", "main"]);
    std::fs::write(seed.join("a.txt"), "base\n").unwrap();
    git_in(&seed, &["add", "-A"]);
    git_in(&seed, &["commit", "-qm", "base"]);
    git_in(
        t,
        &[
            "clone",
            "-q",
            "--bare",
            seed.to_str().unwrap(),
            origin.to_str().unwrap(),
        ],
    );
    let acme = wt.join("acme").join("main");
    git_in(
        t,
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            acme.to_str().unwrap(),
        ],
    );
    std::fs::write(acme.join("a.txt"), "base\nmore\n").unwrap();
    git_in(&acme, &["commit", "-qam", "ahead"]);
    let diff_bn = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_bn"))
            .current_dir(&acme)
            .env("BN_NOTES_DIR", &notes)
            .env("BN_WORKTREE_DIR", &wt)
            .env("BN_HOSTNAME", "h")
            .env("BN_TODAY", "2026-06-20")
            .args(args)
            .output()
            .unwrap()
    };
    diff_bn(&["add", "todo", "ship it"]);
    let out = String::from_utf8_lossy(&diff_bn(&["diff"]).stdout).into_owned();
    assert!(
        out.contains("Changes (acme/main vs main)"),
        "diff header: {out}"
    );
    assert!(out.contains("a.txt"), "shows changed file");
    assert!(out.contains("ship it"), "shows open todo");
    assert!(out.contains("1 commit(s) ahead of main"), "commits ahead");
    let alias = String::from_utf8_lossy(&diff_bn(&["d"]).stdout).into_owned();
    assert!(alias.contains("Changes (acme/main"), "alias d works");

    // --- kill: free a listening port (external process so bn doesn't kill the test) ---
    if Command::new("python3")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        let port = 58231u16;
        let mut listener = Command::new("python3")
            .args([
                "-c",
                &format!(
                    "import socket,time; s=socket.socket(); s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1); s.bind(('127.0.0.1',{port})); s.listen(); time.sleep(60)"
                ),
            ])
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(600));
        let out =
            String::from_utf8_lossy(&diff_bn(&["kill", &port.to_string()]).stdout).into_owned();
        assert!(out.contains("Killing"), "kill reports killing: {out}");
        std::thread::sleep(std::time::Duration::from_millis(400));
        let freed = std::net::TcpListener::bind(("127.0.0.1", port)).is_ok();
        assert!(freed, "port is now free after kill");
        let out2 =
            String::from_utf8_lossy(&diff_bn(&["kill", &port.to_string()]).stdout).into_owned();
        assert!(
            out2.contains("No process listening"),
            "idempotent on free port: {out2}"
        );
        assert!(
            !diff_bn(&["kill", "abc"]).status.success(),
            "non-numeric port rejected"
        );
        let _ = listener.kill();
        let _ = listener.wait();
    }

    // --- clean: remove empty-text frontmatter placeholders -------------------
    let cnotes = t.join("cnotes");
    let cwt = t.join("cwt");
    let cmain = cwt.join("acme").join("main");
    std::fs::create_dir_all(&cmain).unwrap();
    git_in(&cmain, &["init", "-q"]);
    git_in(&cmain, &["checkout", "-q", "-b", "main"]);
    let clean_bn = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_bn"))
            .current_dir(&cmain)
            .env("BN_NOTES_DIR", &cnotes)
            .env("BN_WORKTREE_DIR", &cwt)
            .env("BN_HOSTNAME", "h")
            .env("BN_TODAY", "2026-06-20")
            .args(args)
            .output()
            .unwrap()
    };
    clean_bn(&["add", "todo", "real"]);
    let note = cnotes.join("h").join("acme").join("main").join("note.md");
    let injected = read(&note).replacen(
        "todos:\n",
        "todos:\n- id: t9\n  text: '   '\n  done: false\n  created: '2026-06-20'\n",
        1,
    );
    std::fs::write(&note, injected).unwrap();
    assert!(read(&note).contains("t9"), "empty item present pre-clean");
    let out = String::from_utf8_lossy(&clean_bn(&["clean"]).stdout).into_owned();
    assert!(
        out.contains("removed 1 placeholder"),
        "clean reports removal: {out}"
    );
    assert!(!read(&note).contains("t9"), "empty item gone");
    assert!(read(&note).contains("real"), "real item kept");
    let out = String::from_utf8_lossy(&clean_bn(&["clean"]).stdout).into_owned();
    assert!(out.contains("No placeholders"), "idempotent: {out}");
    let v: serde_json::Value =
        serde_json::from_slice(&clean_bn(&["clean", "--json"]).stdout).unwrap();
    assert_eq!(v["ok"], serde_json::json!(true));
    assert_eq!(v["data"]["removed"], serde_json::json!(0));
}
