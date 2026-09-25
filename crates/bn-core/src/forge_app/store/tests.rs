use super::*;
use crate::forge_app::client::Assignee;
use crate::forge_app::ids::{self, IdMark};
use crate::test_support::forge::{row, FakeForge};

fn store(fake: &Arc<FakeForge>) -> ForgeTaskStore {
    ForgeTaskStore::new(fake.clone())
}

fn find<'a>(file: &'a TodoFile, id: &str) -> &'a Task {
    file.tasks().find(|t| t.id() == Some(id)).expect("task")
}

fn find_mut<'a>(file: &'a mut TodoFile, id: &str) -> &'a mut Task {
    file.task_mut(id).expect("task")
}

#[test]
fn load_maps_every_field() {
    let mut r = row(
        "a1",
        "Ship the thing smart:ok @wip +epic",
        "2026-09-01T08:00:00Z",
        &[
            ("id", "t3"),
            ("stage", "plan"),
            ("branch", "feat/x"),
            ("mod", "99"),
        ],
    );
    r.priority = "Urgent".into();
    r.due_date = Some("2026-09-20".into());
    r.started_at = Some("2026-09-02T08:00:00Z".into());
    r.assignees = vec![Assignee {
        user_id: "u-agent".into(),
        display_name: Some("Agents".into()),
        is_agent: true,
    }];
    let mut done = row("a2", "Done one", "2026-09-02T08:00:00Z", &[("id", "t4")]);
    done.is_completed = true;
    done.completed_at = Some("2026-09-10T12:00:00Z".into());
    done.started_at = Some("2026-09-09T12:00:00Z".into());
    let mut dropped = row("a3", "Dropped", "2026-09-03T08:00:00Z", &[("id", "t5")]);
    dropped.status = "Abandoned".into();
    dropped.goal_repo_url = None;
    let fake = FakeForge::with(vec![done, r, dropped]);
    let file = store(&fake).load().expect("load");

    let t3 = find(&file, "t3");
    assert_eq!(t3.priority, Some('A'));
    assert!(!t3.done);
    assert_eq!(t3.created.as_deref(), Some("2026-09-01"));
    assert_eq!(t3.summary(), "Ship the thing smart-ok wip epic @wip");
    assert_eq!(t3.meta("smart"), None, "a title never carries structure");
    assert_eq!(t3.meta("repo"), Some("eduuh/bn"));
    assert_eq!(t3.meta("stage"), Some("plan"));
    assert_eq!(t3.meta("branch"), Some("feat/x"));
    assert_eq!(t3.meta("due"), Some("2026-09-20"));
    assert_eq!(t3.meta("src"), Some("forge:a1"));
    assert_eq!(t3.meta("uid"), Some("a1"));
    assert_eq!(t3.meta("mod"), None);
    assert_eq!(t3.status("2026-09-14"), crate::todo::Status::Wip);

    let t4 = find(&file, "t4");
    assert!(t4.done);
    assert_eq!(t4.completion.as_deref(), Some("2026-09-10"));
    assert!(!t4.contexts().contains(&"wip"));
    assert_eq!(t4.status("2026-09-14"), crate::todo::Status::Done);

    let t5 = find(&file, "t5");
    assert_eq!(t5.status("2026-09-14"), crate::todo::Status::Cancelled);
    assert_eq!(t5.meta("repo"), None);

    let order: Vec<&str> = file.tasks().filter_map(|t| t.id()).collect();
    assert_eq!(order, ["t3", "t4", "t5"], "ordered by creation");
    assert_eq!(
        fake.take_calls(),
        ["list_bn_tasks Some(7)"],
        "load is one read"
    );
}

#[test]
fn agent_meta_repo_wins_over_the_goal_repository() {
    let fake = FakeForge::with(vec![row(
        "a1",
        "x",
        "2026-09-01",
        &[("id", "t1"), ("repo", "BN")],
    )]);
    let file = store(&fake).load().unwrap();
    assert_eq!(find(&file, "t1").meta("repo"), Some("bn"));
}

#[test]
fn missing_and_duplicate_ids_are_assigned_once_and_stay_stable() {
    let fake = FakeForge::with(vec![
        row("a1", "first", "2026-09-01", &[("id", "t2")]),
        row("a2", "second", "2026-09-02", &[]),
        row("a3", "third", "2026-09-03", &[("id", "t2")]),
        row("a4", "fourth", "2026-09-04", &[("id", "bogus")]),
    ]);
    let s = store(&fake);
    let file = s.load().unwrap();
    let ids: Vec<(&str, &str)> = file
        .tasks()
        .map(|t| (t.meta("src").unwrap(), t.id().unwrap()))
        .collect();
    assert_eq!(
        ids,
        [
            ("forge:a1", "t2"),
            ("forge:a2", "t3"),
            ("forge:a3", "t4"),
            ("forge:a4", "t5")
        ]
    );
    assert_eq!(fake.row("a2").agent_meta.get("id").unwrap(), "t3");
    assert_eq!(fake.row("a3").agent_meta.get("id").unwrap(), "t4");
    let writes = fake
        .take_calls()
        .into_iter()
        .filter(|c| c.starts_with("set_activity_meta"))
        .count();
    assert_eq!(writes, 3);

    let again = store(&fake).load().unwrap();
    let ids_again: Vec<&str> = again.tasks().filter_map(|t| t.id()).collect();
    assert_eq!(ids_again, ["t2", "t3", "t4", "t5"]);
    assert_eq!(
        fake.take_calls(),
        ["list_bn_tasks Some(7)"],
        "no second write-back"
    );
    assert_eq!(s.activity_id("t3").as_deref(), Some("a2"));
}

#[test]
fn an_unchanged_save_sends_nothing_and_a_change_sends_only_itself() {
    let fake = FakeForge::with(vec![
        row("a1", "first", "2026-09-01", &[("id", "t1")]),
        row(
            "a2",
            "second",
            "2026-09-02",
            &[("id", "t2"), ("stage", "plan")],
        ),
    ]);
    let s = store(&fake);
    let mut file = s.load().unwrap();
    fake.take_calls();

    s.save(&file).unwrap();
    assert!(
        fake.take_calls().is_empty(),
        "nothing changed, nothing sent"
    );

    // A meta edit stamps `mod:`, which must not leak into Forge.
    find_mut(&mut file, "t2").set_meta("stage", "execute");
    s.save(&file).unwrap();
    assert_eq!(
        fake.take_calls(),
        [r#"set_activity_meta a2 set={"stage": "execute"} remove=[] expected=Some(1)"#]
    );

    find_mut(&mut file, "t1").priority = Some('B');
    find_mut(&mut file, "t1").set_meta("due", "2026-10-01");
    s.save(&file).unwrap();
    let calls = fake.take_calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(calls[0].starts_with("update_activity a1"), "{calls:?}");
    assert!(calls[0].contains(r#"priority: Some("High")"#), "{calls:?}");
    assert!(
        calls[0].contains(r#"due_date: Some("2026-10-01")"#),
        "{calls:?}"
    );
    assert!(calls[0].contains("title: None"), "{calls:?}");

    find_mut(&mut file, "t1").mark_wip();
    s.save(&file).unwrap();
    assert_eq!(fake.take_calls(), ["set_activity_status a1 started"]);

    let today = "2026-09-14";
    find_mut(&mut file, "t1").mark_done(today);
    find_mut(&mut file, "t2").remove_meta("stage");
    s.save(&file).unwrap();
    let calls = fake.take_calls();
    assert_eq!(
        calls,
        [
            "set_activity_status a1 completed",
            r#"set_activity_meta a2 set={} remove=["stage"] expected=Some(2)"#,
        ]
    );
    assert!(fake.row("a1").is_completed);
    assert!(!fake.row("a2").agent_meta.contains_key("stage"));
}

#[test]
fn a_meta_write_after_an_unversioned_write_fetches_the_version_first() {
    let fake = FakeForge::with(vec![row("a1", "first", "2026-09-01", &[("id", "t1")])]);
    let s = store(&fake);
    let mut file = s.load().unwrap();
    find_mut(&mut file, "t1").priority = Some('A');
    s.save(&file).unwrap();
    fake.take_calls();

    find_mut(&mut file, "t1").set_meta("smart", "ok");
    s.save(&file).unwrap();
    let calls = fake.take_calls();
    assert_eq!(calls[0], "list_bn_tasks Some(7)");
    assert!(calls[1].contains("expected=Some(2)"), "{calls:?}");
}

#[test]
fn a_version_conflict_fails_loudly_and_overwrites_nothing() {
    let fake = FakeForge::with(vec![row(
        "a1",
        "first",
        "2026-09-01",
        &[("id", "t7"), ("stage", "plan")],
    )]);
    let s = store(&fake);
    let mut file = s.load().unwrap();
    *fake.race.lock().unwrap() = Some("a1".into());

    let t = find_mut(&mut file, "t7");
    t.set_meta("stage", "execute");
    t.priority = Some('A');
    let err = s.save(&file).unwrap_err();
    assert!(err.to_string().contains("task t7"), "{err}");
    assert!(err.to_string().contains("nothing was overwritten"), "{err}");

    let r = fake.row("a1");
    assert_eq!(r.agent_meta.get("stage").unwrap(), "plan");
    assert_eq!(r.priority, "None", "the other fields are not pushed either");
    assert_eq!(r.title, "edited elsewhere");
}

#[test]
fn new_tasks_land_under_the_matching_project_goal_or_the_inbox() {
    let fake = FakeForge::with(vec![row("a1", "existing", "2026-09-01", &[("id", "t1")])]);
    let s = store(&fake);
    let mut file = s.load().unwrap();
    fake.take_calls();

    let mut for_bn = Task::new_todo(
        "t2",
        "Fix the gate",
        Some('C'),
        "feat/y",
        "web",
        "2026-09-14",
    );
    for_bn.set_meta("repo", "bn");
    for_bn.set_meta("due", "2026-09-30");
    file.push_task(for_bn);
    let mut elsewhere = Task::new_todo("t3", "Call the bank", None, "", "", "2026-09-14");
    elsewhere.set_meta("repo", "unknown-repo");
    elsewhere.mark_wip();
    file.push_task(elsewhere);

    s.save(&file).unwrap();
    let calls = fake.take_calls();
    assert_eq!(calls[0], "get_goals");
    assert_eq!(
        calls[1],
        r#"create_activity goal=g-bn title=Fix the gate due=Some("2026-09-30") priority=Some("Medium")"#
    );
    assert!(calls[2].starts_with("set_activity_meta new-1"), "{calls:?}");
    assert!(calls[2].contains(r#""id": "t2""#), "{calls:?}");
    assert!(calls[2].contains(r#""branch": "feat/y""#), "{calls:?}");
    assert!(calls[2].contains(r#""origin": "web""#), "{calls:?}");
    assert!(calls[2].contains(r#""uid": "#), "{calls:?}");
    assert!(!calls[2].contains(r#""mod""#), "{calls:?}");
    assert!(!calls[2].contains(r#""due""#), "{calls:?}");
    assert_eq!(
        calls[3],
        "create_activity goal=g-inbox title=Call the bank due=None priority=None"
    );
    assert_eq!(calls[5], "set_activity_status new-2 started");
    assert_eq!(calls.len(), 6, "{calls:?}");

    s.save(&file).unwrap();
    assert!(
        fake.take_calls().is_empty(),
        "saving the same file again does not create twice"
    );

    let reloaded = store(&fake).load().unwrap();
    let t2 = find(&reloaded, "t2");
    assert_eq!(t2.meta("src"), Some("forge:new-1"));
    assert_eq!(t2.meta("repo"), Some("bn"));
    assert_eq!(t2.priority, Some('C'));
    assert_eq!(
        t2.uid(),
        file.tasks().find(|t| t.id() == Some("t2")).unwrap().uid(),
        "identity survives the trip"
    );
}

#[test]
fn append_creates_one_activity() {
    let fake = FakeForge::with(vec![]);
    let s = store(&fake);
    let file = s.load().unwrap();
    let id = file.next_id();
    s.append(&Task::new_todo(
        &id,
        "Only one",
        None,
        "",
        "cli",
        "2026-09-14",
    ))
    .unwrap();
    let back = store(&fake).load().unwrap();
    assert_eq!(back.tasks().count(), 1);
    assert_eq!(find(&back, "t1").summary(), "Only one");
}

#[test]
fn saving_a_task_this_store_never_loaded_is_an_error() {
    let fake = FakeForge::with(vec![row("a1", "x", "2026-09-01", &[("id", "t1")])]);
    let loaded = store(&fake).load().unwrap();
    let err = store(&fake).save(&loaded).unwrap_err();
    assert!(
        err.to_string().contains("not loaded through this store"),
        "{err}"
    );
}

#[test]
fn claim_stamps_repo_and_branch_and_note_comments() {
    let fake = FakeForge::with(vec![row("a1", "x", "2026-09-01", &[("id", "t1")])]);
    let s = store(&fake);
    assert!(s.claim("t1", "BN", "feat/x").unwrap());
    assert!(!s.claim("t9", "bn", "main").unwrap());
    let meta = fake.row("a1").agent_meta;
    assert_eq!(meta.get("repo").unwrap(), "bn");
    assert_eq!(meta.get("branch").unwrap(), "feat/x");

    fake.take_calls();
    s.note("t1", "stage: plan").unwrap();
    assert_eq!(
        fake.take_calls(),
        ["create_comment a1 stage: plan"],
        "a loaded store does not reload to comment"
    );
    assert!(store(&fake).note("t9", "x").is_err());
    let _guard = s.lock();
    assert_eq!(s.kind(), TaskProviderKind::ForgeApp);
}

#[test]
fn without_a_key_every_call_names_bn_forge_login() {
    let s = ForgeTaskStore::without_token();
    let errors = [
        s.load().unwrap_err(),
        s.save(&TodoFile::default()).unwrap_err(),
        s.append(&Task::new_todo("t1", "x", None, "", "", "2026-09-14"))
            .unwrap_err(),
        s.claim("t1", "bn", "main").unwrap_err(),
        s.note("t1", "x").unwrap_err(),
    ];
    for e in errors {
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert!(e.to_string().contains("bn forge login"), "{e}");
    }
    assert_eq!(s.kind(), TaskProviderKind::ForgeApp);
}

#[test]
fn a_claim_between_load_and_save_is_neither_undone_nor_a_conflict() {
    let fake = FakeForge::with(vec![row("a1", "x", "2026-09-01", &[("id", "t1")])]);
    let s = store(&fake);
    let mut file = s.load().unwrap();
    assert!(s.claim("t1", "bn", "feat/x").unwrap());
    fake.take_calls();

    find_mut(&mut file, "t1").set_meta("smart", "ok");
    s.save(&file).unwrap();
    let calls = fake.take_calls();
    assert_eq!(calls[0], "list_bn_tasks Some(7)", "version fetched fresh");
    assert_eq!(
        calls[1],
        r#"set_activity_meta a1 set={"smart": "ok"} remove=[] expected=Some(2)"#
    );
    let meta = fake.row("a1").agent_meta;
    assert_eq!(meta.get("branch").map(String::as_str), Some("feat/x"));
    assert_eq!(meta.get("smart").map(String::as_str), Some("ok"));
}

#[test]
fn ids_of_tasks_outside_the_window_are_never_minted_again() {
    let _env = crate::test_support::env_lock();
    let now = crate::test_support::EnvGuard::set("BN_NOW", "1789000000");
    let dir = tempfile::tempdir().unwrap();
    let mark = || IdMark::new(dir.path().join(ids::FILE), "http://forge.test/mcp");
    let fake = FakeForge::with(vec![row("a1", "open", "2026-09-01", &[("id", "t2")])]);
    let mut old = row("a0", "finished in spring", "2026-03-01", &[("id", "t9")]);
    old.is_completed = true;
    fake.archived.lock().unwrap().push(old);

    // No mark yet: one wide list finds t9, so the file never hands it out.
    let file = store(&fake).with_id_mark(mark()).load().unwrap();
    assert_eq!(
        file.tasks().count(),
        1,
        "the old task stays out of the list"
    );
    assert_eq!(file.next_id(), "t10");
    assert_eq!(
        fake.take_calls(),
        ["list_bn_tasks Some(365)", "list_bn_tasks Some(7)"]
    );
    assert_eq!(mark().get().map(|m| m.max), Some(9));

    // A fresh mark needs no wide list, and an activity with no id gets one past it.
    fake.rows
        .lock()
        .unwrap()
        .push(row("a5", "added in Forge", "2026-09-10", &[]));
    let s = store(&fake).with_id_mark(mark());
    let file = s.load().unwrap();
    assert_eq!(find(&file, "t10").meta("src"), Some("forge:a5"));
    let calls = fake.take_calls();
    assert_eq!(calls[0], "list_bn_tasks Some(7)", "{calls:?}");
    assert_eq!(calls.len(), 2, "list, then the id write-back: {calls:?}");

    // An id a caller mints and creates raises the mark too.
    let id = file.next_id();
    assert_eq!(id, "t11");
    s.append(&Task::new_todo(
        &id,
        "Minted here",
        None,
        "",
        "cli",
        "2026-09-14",
    ))
    .unwrap();
    assert_eq!(mark().get().map(|m| m.max), Some(11));

    // A week without a load: the mark is stale and is refreshed from the wide list again.
    drop(now);
    let _now = crate::test_support::EnvGuard::set(
        "BN_NOW",
        (1_789_000_000 + ids::STALE_SECS + 1).to_string(),
    );
    fake.take_calls();
    store(&fake).with_id_mark(mark()).load().unwrap();
    assert_eq!(fake.take_calls()[0], "list_bn_tasks Some(365)");
}
