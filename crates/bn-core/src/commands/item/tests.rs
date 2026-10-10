use super::*;
use crate::commands::{add, done};
use crate::resolver::Resolved;

const DAY: &str = "2026-10-10";

fn setup(temp: &std::path::Path) -> (Config, Resolved) {
    let cfg = crate::test_support::config(temp);
    let ctx = Resolved {
        repo: "bn".into(),
        branch: "feat".into(),
    };
    std::fs::create_dir_all(cfg.note_dir(&ctx)).unwrap();
    add(&cfg, &ctx, "todo", "write the parser", None, DAY).unwrap();
    add(&cfg, &ctx, "todo", "write the docs", None, DAY).unwrap();
    add(&cfg, &ctx, "blocker", "waiting on review", None, DAY).unwrap();
    (cfg, ctx)
}

fn note(cfg: &Config, ctx: &Resolved) -> Note {
    store::read(&cfg.note_file(ctx)).unwrap()
}

#[test]
fn drop_removes_by_id_in_any_section() {
    let temp = tempfile::tempdir().unwrap();
    let (cfg, ctx) = setup(temp.path());
    let out = drop_item(&cfg, &ctx, "b1").unwrap();
    assert_eq!(
        (out.section, out.text.as_str()),
        (Section::Blocker, "waiting on review")
    );
    assert!(note(&cfg, &ctx).front.items(Section::Blocker).is_empty());
    assert_eq!(note(&cfg, &ctx).front.items(Section::Todo).len(), 2);
}

#[test]
fn substring_must_match_exactly_one_item() {
    let temp = tempfile::tempdir().unwrap();
    let (cfg, ctx) = setup(temp.path());
    assert!(matches!(
        drop_item(&cfg, &ctx, "write"),
        Err(CmdError::Ambiguous(..))
    ));
    assert!(matches!(
        drop_item(&cfg, &ctx, "nope"),
        Err(CmdError::NotFound(_))
    ));
    assert_eq!(drop_item(&cfg, &ctx, "DOCS").unwrap().id, "t2");
}

#[test]
fn undone_reopens_a_done_todo_and_refuses_an_open_one() {
    let temp = tempfile::tempdir().unwrap();
    let (cfg, ctx) = setup(temp.path());
    done(&cfg, &ctx, "t1", DAY, None).unwrap();
    // The substring only searches done items, so "write" is no longer ambiguous.
    assert_eq!(reopen_item(&cfg, &ctx, "write").unwrap().id, "t1");
    let t1 = note(&cfg, &ctx).front.items(Section::Todo)[0].clone();
    assert_eq!((t1.done, t1.done_at), (Some(false), None));
    assert!(matches!(
        reopen_item(&cfg, &ctx, "t1"),
        Err(CmdError::NotDone(_))
    ));
}

#[test]
fn reword_keeps_id_and_state() {
    let temp = tempfile::tempdir().unwrap();
    let (cfg, ctx) = setup(temp.path());
    done(&cfg, &ctx, "t2", DAY, None).unwrap();
    let out = reword_item(&cfg, &ctx, "t2", "  write the user docs ").unwrap();
    assert_eq!(out.previous.as_deref(), Some("write the docs"));
    let t2 = note(&cfg, &ctx).front.items(Section::Todo)[1].clone();
    assert_eq!(
        (t2.id.as_str(), t2.text.as_str(), t2.done),
        ("t2", "write the user docs", Some(true))
    );
    assert!(matches!(
        reword_item(&cfg, &ctx, "t2", " "),
        Err(CmdError::EmptyText)
    ));
}

#[test]
fn every_verb_reports_a_missing_note() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = crate::test_support::config(temp.path());
    let ctx = Resolved {
        repo: "bn".into(),
        branch: "none".into(),
    };
    assert!(matches!(drop_item(&cfg, &ctx, "t1"), Err(CmdError::NoNote)));
    assert!(matches!(
        reopen_item(&cfg, &ctx, "t1"),
        Err(CmdError::NoNote)
    ));
    assert!(matches!(
        reword_item(&cfg, &ctx, "t1", "x"),
        Err(CmdError::NoNote)
    ));
}
