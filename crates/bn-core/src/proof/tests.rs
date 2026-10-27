use super::*;
use crate::model::{Item, Note};
use crate::resolver::{Config, RepoBranch, Resolved};

const AGENT: &str = "bn:feat";
const HUMAN: &str = "human:alice";

fn setup(temp: &std::path::Path) -> (Config, Resolved) {
    let cfg = crate::test_support::config(temp);
    let ctx = Resolved {
        repo: "bn".into(),
        branch: "feat".into(),
    };
    std::fs::create_dir_all(cfg.note_dir(&ctx)).unwrap();
    let mut note = Note::new(
        RepoBranch {
            repo: "bn",
            branch: "feat",
        },
        "2026-09-24",
        "testhost",
    );
    note.front.done_when = vec![item("w1", "tests pass"), item("w2", "docs updated")];
    crate::store::save(&note, &cfg.note_file(&ctx)).unwrap();
    (cfg, ctx)
}

fn item(id: &str, text: &str) -> Item {
    serde_json::from_value(serde_json::json!({
        "id": id, "text": text, "created": "2026-09-24", "done": false
    }))
    .unwrap()
}

fn new(kind: Kind, for_: &[&str], sha: &str) -> NewProof {
    NewProof {
        kind,
        for_: for_.iter().map(|s| s.to_string()).collect(),
        title: "cargo test".into(),
        body: "all green".into(),
        file: None,
        attach_bytes: None,
        truncated: false,
        sha: Some(sha.into()),
        dirty: false,
        author: AGENT.into(),
        source: "bn proof add".into(),
        cmd: Some("cargo test".into()),
        exit: Some(0),
    }
}

fn ready_at(cfg: &Config, ctx: &Resolved, head: &str, f: Freshness) -> Readiness {
    let note = crate::store::read(&cfg.note_file(ctx)).unwrap();
    let listing = list(cfg, ctx);
    let (v, _) = verdicts(cfg, ctx);
    derive(
        &note.front.done_when,
        &listing.proofs,
        &v,
        Some(head),
        f,
        &|_, _| true,
    )
}

#[test]
fn identity_counts_the_worktree_only_under_a_harness() {
    assert_eq!(
        identity(None, None, true, Some("bn:feat"), "me").unwrap(),
        "bn:feat"
    );
    assert_eq!(
        identity(None, None, false, Some("bn:feat"), "Me").unwrap(),
        "human:me"
    );
    assert_eq!(
        identity(Some("human:x"), None, true, Some("bn:feat"), "me").unwrap(),
        "human:x"
    );
    assert_eq!(pid(3), "p3");
    assert_eq!(parse_pid("p3"), Some(3));
    assert_eq!(parse_pid("w3"), None);
}

#[test]
fn add_numbers_files_and_round_trips_frontmatter() {
    let temp = tempfile::tempdir().unwrap();
    let (cfg, ctx) = setup(temp.path());
    let a = add(&cfg, &ctx, &new(Kind::Verify, &["w1"], "abc123"), "t1").unwrap();
    let mut with_log = new(Kind::Log, &[], "abc123");
    with_log.title = "Build log: release!".into();
    with_log.attach_bytes = Some((b"line\n".to_vec(), "log".into()));
    let b = add(&cfg, &ctx, &with_log, "t2").unwrap();
    assert_eq!((a.id.as_str(), b.id.as_str()), ("p1", "p2"));
    assert_eq!(b.file, "p2-build-log-release.md");
    assert_eq!(
        b.attachment.as_deref(),
        Some("p2-build-log-release-file.log")
    );
    assert_eq!(b.bytes, Some(5));
    let listing = list(&cfg, &ctx);
    assert_eq!(listing.proofs.len(), 2);
    assert_eq!(listing.get("p1").unwrap(), &a);
    assert!(
        listing.unregistered.is_empty(),
        "attachments are not strays"
    );
    assert!(cfg.note_dir(&ctx).join("PROOF.md").exists());
}

#[test]
fn add_refuses_unknown_criteria_and_empty_titles() {
    let temp = tempfile::tempdir().unwrap();
    let (cfg, ctx) = setup(temp.path());
    assert!(matches!(
        add(&cfg, &ctx, &new(Kind::Test, &["w9"], "abc"), "t"),
        Err(ProofError::Invalid(_))
    ));
    let mut untitled = new(Kind::Test, &["w1"], "abc");
    untitled.title = " ".into();
    assert!(add(&cfg, &ctx, &untitled, "t").is_err());
}

#[test]
fn nobody_judges_their_own_proof() {
    let temp = tempfile::tempdir().unwrap();
    let (cfg, ctx) = setup(temp.path());
    add(&cfg, &ctx, &new(Kind::Verify, &["w1"], "abc"), "t").unwrap();
    let j = |who, v, why| judge(&cfg, &ctx, "p1", who, v, why, Some("abc"), "t");
    assert!(matches!(
        j(AGENT, Judgement::Accept, "looks good"),
        Err(ProofError::Forbidden(_))
    ));
    assert!(matches!(
        j(HUMAN, Judgement::Withdraw, "nah"),
        Err(ProofError::Forbidden(_))
    ));
    assert!(matches!(
        j(HUMAN, Judgement::Accept, " "),
        Err(ProofError::Invalid(_))
    ));
    assert!(j(HUMAN, Judgement::Accept, "x").is_ok());
    assert!(matches!(
        judge(&cfg, &ctx, "p9", HUMAN, Judgement::Accept, "x", None, "t"),
        Err(ProofError::NotFound(_))
    ));
}

#[test]
fn readiness_follows_evidence_and_verdicts() {
    let temp = tempfile::tempdir().unwrap();
    let (cfg, ctx) = setup(temp.path());
    let state = |head: &str, f| ready_at(&cfg, &ctx, head, f).items[0].state.clone();

    assert_eq!(state("abc", Freshness::Exact), ItemState::Unproven);
    add(&cfg, &ctx, &new(Kind::Verify, &["w1"], "abc"), "t").unwrap();
    assert!(matches!(
        state("abc", Freshness::Exact),
        ItemState::Attached { .. }
    ));

    judge(
        &cfg,
        &ctx,
        "p1",
        HUMAN,
        Judgement::Reject,
        "flaky",
        None,
        "t",
    )
    .unwrap();
    assert!(matches!(
        state("abc", Freshness::Exact),
        ItemState::Rejected { .. }
    ));
    judge(
        &cfg,
        &ctx,
        "p1",
        HUMAN,
        Judgement::Accept,
        "re-ran, fine",
        None,
        "t",
    )
    .unwrap();
    assert_eq!(
        state("abc", Freshness::Exact),
        ItemState::Accepted {
            proof: "p1".into(),
            judge: HUMAN.into()
        }
    );
    // A short head matches a full sha and vice versa.
    assert!(matches!(
        state("abc1234", Freshness::Exact),
        ItemState::Accepted { .. }
    ));
    // A new commit makes it stale under exact, not under ancestor or any.
    assert!(matches!(
        state("def", Freshness::Exact),
        ItemState::Stale { .. }
    ));
    assert!(matches!(
        state("def", Freshness::Ancestor),
        ItemState::Accepted { .. }
    ));
    assert!(matches!(
        state("def", Freshness::Any),
        ItemState::Accepted { .. }
    ));

    // Withdrawn evidence stops counting.
    judge(
        &cfg,
        &ctx,
        "p1",
        AGENT,
        Judgement::Withdraw,
        "wrong branch",
        None,
        "t",
    )
    .unwrap();
    assert_eq!(state("abc", Freshness::Exact), ItemState::Unproven);
    assert!(!ready_at(&cfg, &ctx, "abc", Freshness::Exact).ready);
}

#[test]
fn a_failed_run_never_proves_done_and_hand_ticks_stay_visible() {
    let temp = tempfile::tempdir().unwrap();
    let (cfg, ctx) = setup(temp.path());
    let mut red = new(Kind::Verify, &["w1"], "abc");
    red.exit = Some(101);
    add(&cfg, &ctx, &red, "t").unwrap();
    judge(
        &cfg,
        &ctx,
        "p1",
        HUMAN,
        Judgement::Accept,
        "sure",
        None,
        "t",
    )
    .unwrap();
    assert!(!matches!(
        ready_at(&cfg, &ctx, "abc", Freshness::Exact).items[0].state,
        ItemState::Accepted { .. }
    ));

    crate::commands::done(&cfg, &ctx, "w2", "2026-09-24", None).unwrap();
    let r = ready_at(&cfg, &ctx, "abc", Freshness::Exact);
    let w2 = r.item("w2").unwrap();
    assert!(w2.hand_ticked, "ticked by hand without evidence");
    assert!(!r.ready);
}

#[test]
fn all_criteria_accepted_is_ready() {
    let temp = tempfile::tempdir().unwrap();
    let (cfg, ctx) = setup(temp.path());
    add(&cfg, &ctx, &new(Kind::Verify, &["w1", "w2"], "abc"), "t").unwrap();
    judge(&cfg, &ctx, "p1", HUMAN, Judgement::Accept, "ok", None, "t").unwrap();
    assert!(ready_at(&cfg, &ctx, "abc", Freshness::Exact).ready);
}

#[test]
fn hand_placed_files_are_unregistered_and_bad_verdict_lines_are_counted() {
    let temp = tempfile::tempdir().unwrap();
    let (cfg, ctx) = setup(temp.path());
    add(&cfg, &ctx, &new(Kind::Verify, &["w1"], "abc"), "t").unwrap();
    let d = dir(&cfg, &ctx);
    std::fs::write(d.join("screenshot.png"), b"x").unwrap();
    std::fs::write(d.join("p9-fake.md"), "no frontmatter").unwrap();
    let listing = list(&cfg, &ctx);
    assert_eq!(listing.proofs.len(), 1);
    assert_eq!(listing.unregistered, ["p9-fake.md", "screenshot.png"]);

    judge(&cfg, &ctx, "p1", HUMAN, Judgement::Accept, "ok", None, "t").unwrap();
    let mut log = std::fs::OpenOptions::new()
        .append(true)
        .open(d.join("verdicts.jsonl"))
        .unwrap();
    std::io::Write::write_all(&mut log, b"not json\n").unwrap();
    let (v, bad) = verdicts(&cfg, &ctx);
    assert_eq!((v.len(), bad), (1, 1));
}

#[test]
fn link_is_author_only_and_merges() {
    let temp = tempfile::tempdir().unwrap();
    let (cfg, ctx) = setup(temp.path());
    add(&cfg, &ctx, &new(Kind::Verify, &[], "abc"), "t").unwrap();
    assert!(link(&cfg, &ctx, "p1", &["w1".into()], HUMAN).is_err());
    let p = link(&cfg, &ctx, "p1", &["w1".into(), "w2".into()], AGENT).unwrap();
    let p2 = link(&cfg, &ctx, "p1", &["w1".into()], AGENT).unwrap();
    assert_eq!(p.for_, ["w1", "w2"]);
    assert_eq!(p2.for_, ["w1", "w2"]);
}

#[test]
fn concurrent_adds_get_distinct_ids() {
    let temp = tempfile::tempdir().unwrap();
    let (cfg, ctx) = setup(temp.path());
    // Collect first: every thread must be running before any is joined, or it isn't concurrent.
    #[allow(clippy::needless_collect)]
    let handles: Vec<_> = (0..6)
        .map(|_| {
            let (cfg, ctx) = (cfg.clone(), ctx.clone());
            std::thread::spawn(move || {
                add(&cfg, &ctx, &new(Kind::Test, &[], "abc"), "t")
                    .unwrap()
                    .id
            })
        })
        .collect();
    let mut ids: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 6);
}

#[test]
fn done_on_a_criterion_follows_the_policy() {
    let temp = tempfile::tempdir().unwrap();
    let (mut cfg, ctx) = setup(temp.path());
    // A todo whose text overlaps a criterion: a substring query then isn't a criterion.
    crate::commands::add(&cfg, &ctx, "todo", "update docs index", None, "2026-09-24").unwrap();

    assert_eq!(cfg.proof_done_requires, "warn", "warn is the default");
    assert!(matches!(check_done(&cfg, &ctx, "w1"), DoneCheck::Warn(m) if m.contains("no proof")));
    assert!(matches!(
        check_done(&cfg, &ctx, "tests pass"),
        DoneCheck::Warn(_)
    ));
    assert_eq!(
        check_done(&cfg, &ctx, "docs"),
        DoneCheck::Clear,
        "ambiguous with a todo"
    );
    assert_eq!(
        check_done(&cfg, &ctx, "t1"),
        DoneCheck::Clear,
        "a todo, not a criterion"
    );

    // No git checkout in a test, so no head: only `any` freshness can call evidence current.
    cfg.proof_freshness = "any".into();
    cfg.proof_done_requires = "refuse".into();
    assert!(matches!(check_done(&cfg, &ctx, "w1"), DoneCheck::Refuse(_)));
    add(&cfg, &ctx, &new(Kind::Verify, &["w1"], "abc"), "t").unwrap();
    assert!(
        matches!(check_done(&cfg, &ctx, "w1"), DoneCheck::Refuse(m) if m.contains("nobody has accepted"))
    );
    judge(&cfg, &ctx, "p1", HUMAN, Judgement::Accept, "ok", None, "t").unwrap();
    assert_eq!(check_done(&cfg, &ctx, "w1"), DoneCheck::Clear);

    cfg.proof_done_requires = "off".into();
    assert_eq!(check_done(&cfg, &ctx, "w2"), DoneCheck::Clear);
}

#[test]
fn unknown_commits_are_never_current_under_exact() {
    let temp = tempfile::tempdir().unwrap();
    let (cfg, ctx) = setup(temp.path());
    let mut no_sha = new(Kind::Manual, &["w1"], "abc");
    no_sha.sha = None;
    add(&cfg, &ctx, &no_sha, "t").unwrap();
    judge(&cfg, &ctx, "p1", HUMAN, Judgement::Accept, "ok", None, "t").unwrap();
    let note = crate::store::read(&cfg.note_file(&ctx)).unwrap();
    let listing = list(&cfg, &ctx);
    let (v, _) = verdicts(&cfg, &ctx);
    let at = |head: Option<&str>, f| {
        derive(
            &note.front.done_when,
            &listing.proofs,
            &v,
            head,
            f,
            &|_, _| true,
        )
        .items[0]
            .state
            .clone()
    };
    assert!(matches!(
        at(Some("abc"), Freshness::Exact),
        ItemState::Stale { .. }
    ));
    assert!(matches!(
        at(None, Freshness::Exact),
        ItemState::Stale { .. }
    ));
    assert!(matches!(
        at(None, Freshness::Any),
        ItemState::Accepted { .. }
    ));
}
