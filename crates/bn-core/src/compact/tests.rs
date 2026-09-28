use super::*;
use crate::model::Note;
use crate::resolver::{Config, RepoBranch, Resolved};

fn setup(temp: &std::path::Path) -> (Config, Resolved) {
    let cfg = crate::test_support::config(temp);
    let ctx = Resolved {
        repo: "bn".into(),
        branch: "feat".into(),
    };
    std::fs::create_dir_all(cfg.note_dir(&ctx)).unwrap();
    let note = Note::new(
        RepoBranch {
            repo: "bn",
            branch: "feat",
        },
        "2026-09-25",
        "testhost",
    );
    crate::store::save(&note, &cfg.note_file(&ctx)).unwrap();
    crate::commands::set_goal(&cfg, &ctx, "ship the parser", "2026-09-25").unwrap();
    crate::commands::add(&cfg, &ctx, "todo", "wire the lexer", None, "2026-09-25").unwrap();
    crate::commands::add(
        &cfg,
        &ctx,
        "done-when",
        "parser tests pass",
        None,
        "2026-09-25",
    )
    .unwrap();
    (cfg, ctx)
}

fn entry(ts: i64, phase: Phase, recap: bool) -> Entry {
    Entry {
        ts,
        phase,
        session: None,
        trigger: None,
        ctx_pct: None,
        recap,
        outcome: None,
    }
}

#[test]
fn a_recap_leads_with_the_authors_words_then_the_notes_state() {
    let temp = tempfile::tempdir().unwrap();
    let (cfg, ctx) = setup(temp.path());
    let text = compose(
        &cfg,
        &ctx,
        "halfway through the lexer; the tokenizer is done",
        "t",
    );
    let words = text.find("halfway through the lexer").unwrap();
    let state = text.find("## State").unwrap();
    assert!(words < state);
    for want in [
        "Goal: ship the parser",
        "w1 parser tests pass — unproven",
        "- [ ] t1 wire the lexer",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    let empty = compose(&cfg, &ctx, "  ", "t");
    assert!(empty.contains("(none written"));
}

#[test]
fn writing_a_recap_keeps_the_latest_and_archives_each() {
    let temp = tempfile::tempdir().unwrap();
    let (cfg, ctx) = setup(temp.path());
    write_recap(&cfg, &ctx, "one", "20260925-100000", Some("abcdef123456")).unwrap();
    write_recap(&cfg, &ctx, "two", "20260925-110000", None).unwrap();
    assert_eq!(
        std::fs::read_to_string(recap_path(&cfg, &ctx)).unwrap(),
        "two"
    );
    let archive = cfg.note_dir(&ctx).join("recaps");
    assert!(archive.join("20260925-100000-abcdef12.md").exists());
    assert!(archive.join("20260925-110000.md").exists());
}

#[test]
fn the_log_round_trips_and_skips_bad_lines() {
    let temp = tempfile::tempdir().unwrap();
    let (cfg, ctx) = setup(temp.path());
    append(&cfg, &ctx, &entry(1, Phase::Prep, true)).unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(cfg.note_dir(&ctx).join("compactions.jsonl"))
        .and_then(|mut f| std::io::Write::write_all(&mut f, b"junk\n"))
        .unwrap();
    append(&cfg, &ctx, &entry(2, Phase::Restore, false)).unwrap();
    let log = entries(&cfg, &ctx);
    assert_eq!(log.len(), 2);
    assert_eq!(log[1].phase, Phase::Restore);
}

#[test]
fn a_restore_is_resumed_only_onto_a_fresh_recapped_prep() {
    let prep = |ts, recap| entry(ts, Phase::Prep, recap);
    assert_eq!(
        outcome_for_restore(&[prep(1000, true)], 1100, true),
        Outcome::Resumed
    );
    assert_eq!(
        outcome_for_restore(&[prep(1000, false)], 1100, true),
        Outcome::Rebuilt
    );
    let stale = 1000 + PREP_FRESH_SECS + 1;
    assert_eq!(
        outcome_for_restore(&[prep(1000, true)], stale, true),
        Outcome::Rebuilt
    );
    // A prep already consumed by a restore doesn't count twice.
    let used = [prep(1000, true), entry(1010, Phase::Restore, false)];
    assert_eq!(outcome_for_restore(&used, 1100, true), Outcome::Rebuilt);
    assert_eq!(outcome_for_restore(&[], 1100, true), Outcome::Rebuilt);
    assert_eq!(
        outcome_for_restore(&[prep(1000, true)], 1100, false),
        Outcome::Failed
    );
}
