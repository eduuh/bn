//! Mutating note commands: `add` / `done` and the note-lifecycle verbs. Each engine
//! function reads, mutates, and persists the note, returning a typed outcome; the CLI
//! renders text or JSON. No human printing here.
//!
//! Split into focused submodules; the public verbs and outcome types are re-exported so
//! callers keep using `commands::add`, `commands::close`, `commands::CmdError`, … as
//! before. Screenshot capture (`crate::capture`) and note-store migration
//! (`crate::migrate`) live outside this façade.

mod body;
mod error;
mod note;
mod todo;

pub use body::*;
pub use error::*;
pub use note::*;
pub use todo::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Scalar;
    use crate::resolver::Resolved;
    use crate::store;
    use crate::test_support::config as cfg;

    fn ctx() -> Resolved {
        Resolved {
            repo: "demo".into(),
            branch: "main".into(),
        }
    }

    #[test]
    fn add_creates_note_and_assigns_ids() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let x = ctx();
        let a = add(&c, &x, "todo", "first thing", None, "2026-06-19").unwrap();
        assert_eq!(a.id, "t1");
        assert!(a.line > 0);
        let b = add(&c, &x, "todo", "second", None, "2026-06-19").unwrap();
        assert_eq!(b.id, "t2");
        // decision gets its own counter and no done field
        let d = add(&c, &x, "decision", "use serde", None, "2026-06-19").unwrap();
        assert_eq!(d.id, "d1");
        let note = store::read(&c.note_file(&x)).unwrap();
        assert_eq!(note.front.decisions[0].done, None);
        assert_eq!(note.front.todos[0].done, Some(false));
    }

    #[test]
    fn concurrent_body_appends_never_lose_an_update() {
        // Regression for the unlocked read-modify-write: many writers append to the same note at
        // once. Every bullet must survive — an unlocked read/mutate/save would let one writer's
        // save clobber another's (the note is atomic, so no torn file, just a silent lost update).
        use std::sync::{Arc, Barrier};

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let x = ctx();
        // Seed the note so every writer takes the read-modify-write path, not the create path.
        append_body(&cfg(&root), &x, "progress", "seed", "2026-06-19").unwrap();

        const WRITERS: usize = 12;
        let barrier = Arc::new(Barrier::new(WRITERS));
        let mut handles = Vec::with_capacity(WRITERS);
        for i in 0..WRITERS {
            let root = root.clone();
            let x = x.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let c = cfg(&root);
                barrier.wait(); // release all writers together to force real contention
                append_body(&c, &x, "progress", &format!("finding-{i}"), "2026-06-19").unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let note = store::read(&cfg(&root).note_file(&x)).unwrap();
        for i in 0..WRITERS {
            assert!(
                note.body.contains(&format!("finding-{i}")),
                "lost update: finding-{i} missing — a concurrent read-modify-write clobbered it"
            );
        }
    }

    #[test]
    fn done_when_criteria_are_checkable_and_kept_out_of_todos() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let x = ctx();
        // Criteria get their own `w` counter and seed an unchecked checkbox.
        let a = add(&c, &x, "done-when", "tests pass", None, "2026-06-19").unwrap();
        assert_eq!(a.id, "w1");
        let note = store::read(&c.note_file(&x)).unwrap();
        assert_eq!(note.front.done_when[0].done, Some(false));
        assert!(
            note.front.todos.is_empty(),
            "criteria must not land in todos"
        );

        // `bn done w1` checks the criterion off, not a todo.
        done(&c, &x, "w1", "2026-06-20", None).unwrap();
        let note = store::read(&c.note_file(&x)).unwrap();
        assert_eq!(note.front.done_when[0].done, Some(true));
        assert_eq!(
            note.front.done_when[0].done_at.as_deref(),
            Some("2026-06-20")
        );
    }

    #[test]
    fn add_tagged_sets_quarter_key_on_the_item() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let x = ctx();
        // Untagged add leaves q absent (round-trips unchanged).
        add(&c, &x, "todo", "plain", None, "2026-06-19").unwrap();
        // Tagged add stamps the quarter key onto the item.
        add_tagged(
            &c,
            &x,
            "todo",
            "cut v1 tag",
            None,
            Some("ship-v1"),
            "2026-06-19",
        )
        .unwrap();
        let note = store::read(&c.note_file(&x)).unwrap();
        assert_eq!(note.front.todos[0].q, None);
        assert_eq!(note.front.todos[1].q.as_deref(), Some("ship-v1"));
        // The tag is greppable in the persisted YAML.
        assert!(std::fs::read_to_string(c.note_file(&x))
            .unwrap()
            .contains("q: ship-v1"));
    }

    #[test]
    fn close_warns_on_unmet_criteria_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let x = ctx();
        add(&c, &x, "done-when", "ship the docs", None, "2026-06-19").unwrap();
        add(&c, &x, "done-when", "green CI", None, "2026-06-19").unwrap();
        done(&c, &x, "w2", "2026-06-20", None).unwrap(); // meet one, leave one

        let out = close(&c, &x, "2026-06-21", None).unwrap();
        assert_eq!(out.unmet_criteria, vec!["ship the docs".to_string()]);
        // Warn-only: the note still closes.
        let note = store::read(&c.note_file(&x)).unwrap();
        assert_eq!(note.front.status, "closed");
        // Criteria are never auto-completed — the unmet one stays unchecked.
        assert_eq!(note.front.done_when[0].done, Some(false));
    }

    #[test]
    fn add_if_absent_dedups_on_key() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let x = ctx();
        let first = add(
            &c,
            &x,
            "todo",
            "[#42] Fix login",
            Some("[#42]"),
            "2026-06-19",
        )
        .unwrap();
        assert!(!first.skipped);
        assert_eq!(first.id, "t1");
        // same key, different title text → skipped, points at the existing item, nothing written
        let again = add(
            &c,
            &x,
            "todo",
            "[#42] Fix login bug",
            Some("[#42]"),
            "2026-06-19",
        )
        .unwrap();
        assert!(again.skipped);
        assert_eq!(again.id, "t1");
        let note = store::read(&c.note_file(&x)).unwrap();
        assert_eq!(note.front.todos.len(), 1, "no duplicate appended");
        // a different key still adds
        let other = add(
            &c,
            &x,
            "todo",
            "[#43] New thing",
            Some("[#43]"),
            "2026-06-19",
        )
        .unwrap();
        assert!(!other.skipped);
        assert_eq!(other.id, "t2");
    }

    #[test]
    fn add_if_absent_matches_done_items() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let x = ctx();
        add(&c, &x, "todo", "[#7] ship it", Some("[#7]"), "2026-06-19").unwrap();
        done(&c, &x, "t1", "2026-06-20", None).unwrap();
        // a completed pull must not be resurrected on the next pull
        let again = add(&c, &x, "todo", "[#7] ship it", Some("[#7]"), "2026-06-21").unwrap();
        assert!(again.skipped);
        assert_eq!(store::read(&c.note_file(&x)).unwrap().front.todos.len(), 1);
    }

    #[test]
    fn add_unknown_section_errors() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let err = add(&c, &ctx(), "notes", "x", None, "2026-06-19").unwrap_err();
        assert_eq!(err.code(), "unknown_section");
    }

    #[test]
    fn done_by_substring_then_already_done() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let x = ctx();
        add(&c, &x, "todo", "wire clap skeleton", None, "2026-06-19").unwrap();
        add(&c, &x, "todo", "port resolver", None, "2026-06-19").unwrap();
        let d = done(&c, &x, "clap", "2026-06-20", None).unwrap();
        assert_eq!(d.id, "t1");
        // second time → already done
        assert_eq!(
            done(&c, &x, "t1", "2026-06-20", None).unwrap_err().code(),
            "already_done"
        );
    }

    #[test]
    fn done_pct_records_progress_without_closing() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let x = ctx();
        add(&c, &x, "todo", "long-running migration", None, "2026-06-19").unwrap();

        // Partial: stays open, pct recorded and round-trips through the store.
        let d = done(&c, &x, "t1", "2026-06-20", Some(60)).unwrap();
        assert_eq!(d.pct, Some(60));
        let note = store::read(&c.note_file(&x)).unwrap();
        assert_eq!(note.front.todos[0].done, Some(false), "still open");
        assert_eq!(note.front.todos[0].pct, Some(60));

        // A later pct overwrites; 100 completes and clears the pct.
        done(&c, &x, "t1", "2026-06-21", Some(90)).unwrap();
        let d = done(&c, &x, "t1", "2026-06-22", Some(100)).unwrap();
        assert_eq!(d.pct, None, "100 means completed");
        let note = store::read(&c.note_file(&x)).unwrap();
        assert_eq!(note.front.todos[0].done, Some(true));
        assert_eq!(note.front.todos[0].pct, None, "cleared on completion");

        // Progressing an already-done item still errors.
        assert_eq!(
            done(&c, &x, "t1", "2026-06-23", Some(10))
                .unwrap_err()
                .code(),
            "already_done"
        );
    }

    #[test]
    fn done_ambiguous_and_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let x = ctx();
        add(&c, &x, "todo", "fix the bug", None, "2026-06-19").unwrap();
        add(&c, &x, "todo", "fix the test", None, "2026-06-19").unwrap();
        assert_eq!(
            done(&c, &x, "fix", "2026-06-20", None).unwrap_err().code(),
            "ambiguous"
        );
        assert_eq!(
            done(&c, &x, "nope", "2026-06-20", None).unwrap_err().code(),
            "not_found"
        );
    }

    #[test]
    fn note_path_creates_note_and_returns_note_md() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let x = ctx();
        let p = note_path(&c, &x, "2026-06-21").unwrap();
        assert!(p.ends_with("note.md"));
        assert!(p.exists());
    }

    #[test]
    fn list_files_excludes_note_md_and_sorts() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let x = ctx();
        let nd = ensure_note(&c, &x, "2026-06-21").unwrap();
        std::fs::write(nd.join("plan.md"), "p").unwrap();
        std::fs::write(nd.join("debug.md"), "d").unwrap();
        let (returned_dir, files) = list_files(&c, &x);
        assert_eq!(returned_dir, nd);
        let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["debug.md", "plan.md"]); // sorted, note.md excluded
        assert!(files.iter().all(|f| f.path.ends_with(&f.name)));
    }

    #[test]
    fn ensure_note_creates_then_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let x = ctx();
        let p = c.note_file(&x);
        assert!(!p.exists());
        ensure_note(&c, &x, "2026-06-20").unwrap();
        assert!(p.exists());
        let before = std::fs::read_to_string(&p).unwrap();
        ensure_note(&c, &x, "2026-06-21").unwrap(); // existing note untouched
        assert_eq!(std::fs::read_to_string(&p).unwrap(), before);
    }

    #[test]
    fn set_status_close_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let x = ctx();
        add(&c, &x, "todo", "x", None, "2026-06-20").unwrap();
        set_status(&c, &x, "closed").unwrap();
        assert_eq!(
            store::read(&c.note_file(&x)).unwrap().front.status,
            "closed"
        );
        set_status(&c, &x, "active").unwrap();
        assert_eq!(
            store::read(&c.note_file(&x)).unwrap().front.status,
            "active"
        );
        // closing a nonexistent note errors
        let y = Resolved {
            repo: "nope".into(),
            branch: "main".into(),
        };
        assert_eq!(set_status(&c, &y, "closed").unwrap_err().code(), "no_note");
    }

    #[test]
    fn close_completes_todos_sets_status_and_logs_goal_achievement() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let x = ctx();
        add(&c, &x, "todo", "wire it", None, "2026-06-20").unwrap();
        add(&c, &x, "todo", "test it", None, "2026-06-20").unwrap();
        done(&c, &x, "wire", "2026-06-20", None).unwrap(); // one already done
        set_goal(&c, &x, "Ship the widget", "2026-06-20").unwrap();
        set_pr(&c, &x, "https://gh/pr/7", "2026-06-20").unwrap();

        let o = close(&c, &x, "2026-06-22", None).unwrap();
        assert_eq!(o.todos_completed, 1); // only the still-open one
        assert_eq!(
            o.achievement.as_deref(),
            Some("Ship the widget (https://gh/pr/7)")
        );

        let note = store::read(&c.note_file(&x)).unwrap();
        assert_eq!(note.front.status, "closed");
        assert!(note.front.todos.iter().all(|t| t.done == Some(true)));
        assert_eq!(note.front.todos[1].done_at.as_deref(), Some("2026-06-22"));

        let log = std::fs::read_to_string(c.note_dir(&x).join("achievements.md")).unwrap();
        assert!(log.contains("# Achievements"), "{log}");
        assert!(
            log.contains("- 2026-06-22 — Ship the widget (https://gh/pr/7)"),
            "{log}"
        );
    }

    #[test]
    fn reclose_is_idempotent_and_does_not_reappend_achievement() {
        // The close->cleanup chain (and a double cleanup pass) calls close twice. The second call
        // must not append the achievement again or report one it didn't log.
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let x = ctx();
        set_goal(&c, &x, "Ship the widget", "2026-06-20").unwrap();

        let first = close(&c, &x, "2026-06-22", None).unwrap();
        assert_eq!(first.achievement.as_deref(), Some("Ship the widget"));

        let second = close(&c, &x, "2026-06-23", None).unwrap();
        assert_eq!(second.achievement, None, "re-close logged nothing");

        let log = std::fs::read_to_string(c.note_dir(&x).join("achievements.md")).unwrap();
        assert_eq!(
            log.matches("Ship the widget").count(),
            1,
            "achievement appended exactly once: {log}"
        );
    }

    #[test]
    fn close_uses_explicit_achievement_and_no_goal_logs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let x = ctx();
        // no goal set → derived achievement is None
        add(&c, &x, "todo", "do thing", None, "2026-06-20").unwrap();
        let o = close(&c, &x, "2026-06-22", None).unwrap();
        assert_eq!(o.achievement, None);
        assert!(!c.note_dir(&x).join("achievements.md").exists());

        // explicit override is logged even without a goal; reopening then closing appends
        set_status(&c, &x, "active").unwrap();
        let o2 = close(&c, &x, "2026-06-23", Some("Did the thing")).unwrap();
        assert_eq!(o2.achievement.as_deref(), Some("Did the thing"));
        let log = std::fs::read_to_string(c.note_dir(&x).join("achievements.md")).unwrap();
        assert!(log.contains("- 2026-06-23 — Did the thing"), "{log}");

        // closing a nonexistent note errors
        let y = Resolved {
            repo: "nope".into(),
            branch: "main".into(),
        };
        assert_eq!(
            close(&c, &y, "2026-06-22", None).unwrap_err().code(),
            "no_note"
        );
    }

    #[test]
    fn set_pr_creates_note_then_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let x = ctx();
        // creates the note when none exists, unlike set_status
        assert!(!c.note_file(&x).exists());
        let out = set_pr(&c, &x, "https://github.com/eduuh/bn/pull/2", "2026-06-20").unwrap();
        assert_eq!(out.pr, "https://github.com/eduuh/bn/pull/2");
        assert_eq!(
            store::read(&c.note_file(&x)).unwrap().front.pr,
            Some(Scalar::Str("https://github.com/eduuh/bn/pull/2".into()))
        );
        // overwrites an existing value
        set_pr(&c, &x, "https://github.com/eduuh/bn/pull/3", "2026-06-21").unwrap();
        assert_eq!(
            store::read(&c.note_file(&x)).unwrap().front.pr,
            Some(Scalar::Str("https://github.com/eduuh/bn/pull/3".into()))
        );
        // empty input rejected
        assert_eq!(
            set_pr(&c, &x, "  ", "2026-06-21").unwrap_err().code(),
            "empty_text"
        );
    }

    #[test]
    fn record_plan_writes_a_verbatim_plan_section_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let x = ctx();
        record_plan(
            &c,
            &x,
            "## Approach\nDo the thing.\n## Files\nsrc/a.rs",
            "2026-06-20",
        )
        .unwrap();
        let body = store::read(&c.note_file(&x)).unwrap().body;
        // The `## Plan` section exists exactly once and the plan's own headings are demoted below
        // level 2 so they don't collide with the note's section delimiters.
        assert_eq!(body.matches("## Plan").count(), 1);
        assert!(body.contains("#### Approach"));
        assert!(body.contains("#### Files"));
        // Re-recording (e.g. approve after an edit) replaces rather than stacks.
        record_plan(&c, &x, "## Approach\nRevised.", "2026-06-21").unwrap();
        let body2 = store::read(&c.note_file(&x)).unwrap().body;
        assert_eq!(body2.matches("## Plan").count(), 1);
        assert!(body2.contains("Revised."));
        assert!(!body2.contains("Do the thing."));
        // Empty plan is rejected.
        assert_eq!(
            record_plan(&c, &x, "   \n", "2026-06-21")
                .unwrap_err()
                .code(),
            "empty_text"
        );
    }

    #[test]
    fn append_body_grows_prose_sections_and_lists_them() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let x = ctx();
        // append creates the note and the section, then appends in order
        let o = append_body(&c, &x, "notes", "first note", "2026-06-20").unwrap();
        assert_eq!(o.label, "Notes");
        append_body(&c, &x, "notes", "second note", "2026-06-20").unwrap();
        append_body(&c, &x, "tc", "login works", "2026-06-20").unwrap();

        let notes = read_body_section(&c, &x, "notes").unwrap();
        assert_eq!(notes, vec!["- first note", "- second note"]);
        let tcs = read_body_section(&c, &x, "tc").unwrap();
        assert_eq!(tcs, vec!["- login works"]);
        // notes and tc are independent sections
        assert!(!notes.iter().any(|l| l.contains("login")));

        // unknown section + empty text rejected
        assert_eq!(
            append_body(&c, &x, "bogus", "x", "2026-06-20")
                .unwrap_err()
                .code(),
            "unknown_section"
        );
        assert_eq!(
            append_body(&c, &x, "notes", "  ", "2026-06-20")
                .unwrap_err()
                .code(),
            "empty_text"
        );
        // listing a missing note errors
        let y = Resolved {
            repo: "nope".into(),
            branch: "main".into(),
        };
        assert_eq!(
            read_body_section(&c, &y, "notes").unwrap_err().code(),
            "no_note"
        );
    }

    #[test]
    fn set_work_item_stores_numeric_as_int_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let x = ctx();
        // numeric id → stored as a YAML number
        let out = set_work_item(&c, &x, "12345", "2026-06-20").unwrap();
        assert_eq!(out.work_item, "12345");
        assert_eq!(
            store::read(&c.note_file(&x)).unwrap().front.work_item,
            Some(Scalar::Int(12345))
        );
        assert_eq!(read_work_item(&c, &x).as_deref(), Some("12345"));
        // non-numeric id → stored as a string; overwrites
        set_work_item(&c, &x, "AB-99", "2026-06-21").unwrap();
        assert_eq!(
            store::read(&c.note_file(&x)).unwrap().front.work_item,
            Some(Scalar::Str("AB-99".into()))
        );
        // empty rejected
        assert_eq!(
            set_work_item(&c, &x, "  ", "2026-06-21")
                .unwrap_err()
                .code(),
            "empty_text"
        );
    }

    #[test]
    fn read_pr_and_work_item_none_without_note_or_value() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let x = ctx();
        // no note at all
        assert_eq!(read_pr(&c, &x), None);
        assert_eq!(read_work_item(&c, &x), None);
        // note exists but no pr/work_item set
        add(&c, &x, "todo", "x", None, "2026-06-20").unwrap();
        assert_eq!(read_pr(&c, &x), None);
        assert_eq!(read_work_item(&c, &x), None);
        // after set
        set_pr(&c, &x, "https://example/pr/1", "2026-06-20").unwrap();
        assert_eq!(read_pr(&c, &x).as_deref(), Some("https://example/pr/1"));
    }

    #[test]
    fn done_migrates_split_note_and_clears_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let nd = dir.path().join("testhost").join("demo").join("main");
        std::fs::create_dir_all(&nd).unwrap();
        std::fs::write(nd.join("note.md"), "# demo / main\n\n## Goal\n\nx\n").unwrap();
        std::fs::write(
            nd.join("note.yaml"),
            "repo: demo\nbranch: main\ncreated: 2026-04-17\nstatus: active\nmachine: h\ntags: []\ncollaborators: []\ntodos:\n  - id: t1\n    text: legacy todo\n    done: false\n    added: \"2026-04-17\"\nblockers: []\ndecisions: []\nresearch: []\ncollab: []\nask: []\n",
        )
        .unwrap();
        let c = cfg(dir.path());
        let d = done(&c, &ctx(), "legacy", "2026-06-20", None).unwrap();
        assert_eq!(d.id, "t1");
        // sibling retired so the migrated single-file is authoritative on re-read
        assert!(!nd.join("note.yaml").exists());
        assert!(nd.join("note.yaml.bak").exists());
        let back = store::read(&nd.join("note.md")).unwrap();
        assert_eq!(back.front.todos[0].done, Some(true));
        assert_eq!(back.front.todos[0].done_at.as_deref(), Some("2026-06-20"));
    }

    #[test]
    fn est_persists_and_close_stamps_actual_from_track() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let x = ctx(); // demo / main
                       // 1h of tracked time on demo/main, opened and closed on the todo's day.
        let day = "2026-06-20";
        let t0 = crate::track::parse_at(day, "09:00").unwrap();
        crate::track::punch_in(&c, day, t0, "work", Some("demo"), Some("main"), None, None)
            .unwrap();
        crate::track::punch_out(&c, day, t0 + 3600).unwrap();

        let a = add_est(&c, &x, "todo", "refactor store", None, Some("30m"), day).unwrap();
        // Grammar round-trip: the estimate survives save → read; actual only lands at close.
        let t = store::read(&c.note_file(&x)).unwrap().front.todos[0].clone();
        assert_eq!(t.est.as_deref(), Some("30m"));
        assert!(t.actual.is_none());

        // Closing stamps actual = summed punch time (1h), both returned and persisted.
        let out = done(&c, &x, "refactor", day, None).unwrap();
        assert_eq!(out.actual.as_deref(), Some("1h00m"));
        let t = store::read(&c.note_file(&x))
            .unwrap()
            .front
            .todos
            .into_iter()
            .find(|t| t.id == a.id)
            .unwrap();
        assert_eq!(t.est.as_deref(), Some("30m"));
        assert_eq!(t.actual.as_deref(), Some("1h00m"));
    }

    #[test]
    fn done_without_est_stamps_no_actual_even_with_punches() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let x = ctx();
        let day = "2026-06-20";
        let t0 = crate::track::parse_at(day, "09:00").unwrap();
        crate::track::punch_in(&c, day, t0, "work", Some("demo"), Some("main"), None, None)
            .unwrap();
        crate::track::punch_out(&c, day, t0 + 3600).unwrap();
        add(&c, &x, "todo", "no estimate", None, day).unwrap();
        assert!(done(&c, &x, "no estimate", day, None)
            .unwrap()
            .actual
            .is_none());
    }

    #[test]
    fn done_with_est_but_no_matching_punches_skips_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path()); // no punch log at all
        let x = ctx();
        add_est(&c, &x, "todo", "estimated", None, Some("2h"), "2026-06-20").unwrap();
        let out = done(&c, &x, "estimated", "2026-06-21", None).unwrap();
        assert!(out.actual.is_none(), "empty window records nothing");
        let t = &store::read(&c.note_file(&x)).unwrap().front.todos[0];
        assert!(t.actual.is_none());
    }

    #[test]
    fn unestimated_todo_serializes_without_est_actual_keys() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let x = ctx();
        add(&c, &x, "todo", "plain todo", None, "2026-06-20").unwrap();
        let raw = std::fs::read_to_string(c.note_file(&x)).unwrap();
        assert!(
            !raw.contains("est:"),
            "no est key when un-estimated:\n{raw}"
        );
        assert!(
            !raw.contains("actual:"),
            "no actual key when un-estimated:\n{raw}"
        );
    }
}
