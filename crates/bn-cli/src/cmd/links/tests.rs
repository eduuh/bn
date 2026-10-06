use super::*;

fn link(name: &str, url: &str) -> Link {
    Link {
        id: "k1".into(),
        url: url.into(),
        name: name.into(),
        tags: vec!["dri".into()],
        repo: Some("bn".into()),
        branch: None,
        created: "2026-01-01".into(),
        uses: 0,
        last: None,
        new: false,
        expires: None,
    }
}

#[test]
fn a_picker_row_carries_the_id_after_the_last_tab() {
    let l = link("Prod subscription", "https://portal.azure.com/x");
    let row = format!("{}{TAB}{}", render(&l), l.id);
    assert_eq!(row.rsplit(TAB).next().unwrap(), "k1");
}

#[test]
fn everything_searchable_is_in_the_row_fzf_matches_on() {
    let display = render(&link("Prod subscription", "https://portal.azure.com/x"));
    for needle in ["Prod subscription", "bn", "dri", "portal.azure.com"] {
        assert!(display.contains(needle), "row is missing {needle}: {display}");
    }
}

#[test]
fn a_group_row_comes_back_as_its_tag_key_beside_plain_ids() {
    let out = format!("\n▸ #standup (2 links){TAB}#standup\nProd{TAB}k7\n");
    let (_, keys) = parse_pick(&out).unwrap();
    assert_eq!(keys, ["#standup", "k7"]);
}

#[test]
fn a_group_key_expands_to_its_members_and_a_link_opens_once() {
    let mut a = link("a", "https://a");
    a.id = "k1".into();
    a.tags = vec!["standup".into()];
    let mut b = link("b", "https://b");
    b.id = "k2".into();
    b.tags = vec!["standup".into()];
    let items = vec![a, b];
    let ids = expand(&items, &["k2".into(), "#standup".into()]);
    assert_eq!(ids, ["k2", "k1"]);
}

#[test]
fn a_group_sits_just_above_its_best_ranked_member() {
    let mk = |id: &str, tags: &[&str]| {
        let mut l = link(id, &format!("https://{id}"));
        l.id = id.into();
        l.tags = tags.iter().map(|t| t.to_string()).collect();
        l
    };
    let items = [mk("k1", &[]), mk("k2", &["standup"]), mk("k3", &["standup"])];
    let ranked: Vec<&Link> = items.iter().collect();
    let keys: Vec<String> = rows(&ranked)
        .iter()
        .map(|r| r.rsplit(TAB).next().unwrap().to_string())
        .collect();
    assert_eq!(keys, ["k1", "#standup", "k2", "k3"]);
}

#[test]
fn only_what_was_said_is_harvested_never_tool_output() {
    let jsonl = [
        r#"{"type":"user","message":{"content":"look at https://github.com/o/r/pull/1"}}"#,
        r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"https://github.com/o/r/pull/2"}]}}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Opened https://github.com/o/r/pull/3"},{"type":"tool_use","input":{"cmd":"https://github.com/o/r/pull/4"}}]}}"#,
    ]
    .join("\n");
    let said = spoken_text(&jsonl);
    assert!(said.contains("pull/1") && said.contains("pull/3"));
    assert!(!said.contains("pull/2") && !said.contains("pull/4"));
}

#[test]
fn tag_changes_split_into_adds_and_drops() {
    let (add, drop) = split_changes(&["+standup -dri".into(), "#docs,-#icm".into(), "-".into()]);
    assert_eq!(add, ["standup", "docs"]);
    assert_eq!(drop, ["dri", "icm"]);
}

#[test]
fn a_long_name_is_cut_with_an_ellipsis_not_silently() {
    assert_eq!(truncate("abcdefghij", 5), "abcd…");
    assert_eq!(truncate("abc", 5), "abc");
}

#[test]
fn one_link_shares_as_a_single_escaped_hyperlink() {
    let l = link("ICM <sev2> & co", "https://x.test/?a=1&b=\"2\"");
    let (html, text) = share_payload(&[&l]);
    assert_eq!(
        html,
        r#"<a href="https://x.test/?a=1&amp;b=&quot;2&quot;">ICM &lt;sev2&gt; &amp; co</a>"#
    );
    assert_eq!(text, "ICM <sev2> & co https://x.test/?a=1&b=\"2\"");
}

#[test]
fn several_links_share_as_a_list() {
    let a = link("A", "https://a.test");
    let b = link("B", "https://b.test");
    let (html, text) = share_payload(&[&a, &b]);
    assert_eq!(
        html,
        r#"<ul><li><a href="https://a.test">A</a></li><li><a href="https://b.test">B</a></li></ul>"#
    );
    assert_eq!(text, "A https://a.test\nB https://b.test");
}

#[test]
fn fzf_expect_output_maps_to_an_action_and_ids() {
    assert_eq!(
        parse_pick("\nrow one\tk3\n"),
        Some((PickAction::Open, vec!["k3".to_string()]))
    );
    assert_eq!(
        parse_pick("ctrl-y\nrow\tk1\nrow\tk7\n"),
        Some((PickAction::Copy, vec!["k1".to_string(), "k7".to_string()]))
    );
    assert_eq!(parse_pick("ctrl-y\n"), None);
    assert_eq!(
        parse_pick("ctrl-t\nrow\t#standup\n"),
        Some((PickAction::Tag, vec!["#standup".to_string()]))
    );
    assert_eq!(parse_pick("ctrl-d\nrow\tk2\n").unwrap().0, PickAction::Delete);
    assert_eq!(parse_pick("ctrl-e\nrow\tk2\n").unwrap().0, PickAction::Rename);
}

#[test]
fn a_new_link_is_marked_in_the_picker_row() {
    let mut l = link("PR 7", "https://github.com/o/r/pull/7");
    assert!(!render(&l).contains("•new"));
    l.new = true;
    assert!(render(&l).contains("•new"));
}

#[test]
fn copilot_events_yield_messages_and_skip_tool_events() {
    let jsonl = [
        r#"{"type":"user.message","data":{"content":"see https://github.com/o/r/pull/1"}}"#,
        r#"{"type":"tool.execution_complete","data":{"result":"https://github.com/o/r/pull/2"}}"#,
        r#"{"type":"assistant.message","data":{"content":"Opened https://github.com/o/r/pull/3"}}"#,
        r#"{"type":"hook.start","data":{}}"#,
    ]
    .join("\n");
    let said = spoken_text(&jsonl);
    assert!(said.contains("pull/1") && said.contains("pull/3"));
    assert!(!said.contains("pull/2"));
}

#[test]
fn a_copilot_session_id_cannot_climb_out_of_session_state() {
    assert!(copilot_events("80b2f300-1330-42a6-949d-58744f052fc3").is_some());
    assert!(copilot_events("../../etc").is_none());
    assert!(copilot_events("").is_none());
}
