use super::*;

fn vocab() -> Vec<String> {
    DEFAULT_TAGS.iter().map(|t| t.to_string()).collect()
}

#[test]
fn guess_tags_by_host() {
    let v = vocab();
    let g = |u: &str| guess_tags(u, &v);
    assert_eq!(
        g("https://portal.example-incidents.com/imp/v3/incidents/1"),
        ["icm"]
    );
    assert_eq!(g("https://incidents.example.net/x"), ["icm"]);
    assert_eq!(g("https://learn.microsoft.com/en-us/azure"), ["docs"]);
    assert_eq!(g("https://docs.example.com/docs/x"), ["docs"]);
    assert_eq!(g("https://docs.rs/serde"), ["docs"]);
    assert_eq!(g("https://tokio.readthedocs.io/"), ["docs"]);
    assert_eq!(
        g("https://myaccess.microsoft.com/@x#/access-packages"),
        ["permissions"]
    );
    assert_eq!(g("https://groups.example/GroupManagement"), ["permissions"]);
    assert_eq!(g("https://security.microsoft.com:443/alerts"), ["security"]);
    assert!(g("https://github.com/eduuh/bn/pull/1").is_empty());
    // `.suffix` must match on a label boundary, not any substring.
    assert!(g("https://notexample-incidents.com/").is_empty());
}

#[test]
fn guess_tags_respects_vocabulary() {
    let v = vec!["security".to_string()];
    assert!(guess_tags("https://portal.example-incidents.com/x", &v).is_empty());
    assert_eq!(
        guess_tags("https://security.microsoft.com/", &v),
        ["security"]
    );
}

fn link(name: &str, uses: u32, last: Option<&str>) -> Link {
    Link {
        id: format!("k{name}"),
        url: format!("https://example.com/{name}"),
        name: name.to_string(),
        tags: vec![],
        repo: None,
        branch: None,
        created: "2026-01-01".to_string(),
        uses,
        last: last.map(str::to_string),
        new: false,
        expires: None,
    }
}

fn tagged(name: &str, tags: &[&str]) -> Link {
    Link {
        tags: tags.iter().map(|t| t.to_string()).collect(),
        ..link(name, 0, None)
    }
}

#[test]
fn a_tag_on_two_links_is_a_group_and_a_tag_on_one_is_not() {
    let items = [
        tagged("a", &["standup", "dri"]),
        tagged("b", &["standup"]),
        tagged("c", &["standup"]),
        tagged("d", &["dri", "solo"]),
    ];
    let refs: Vec<&Link> = items.iter().collect();
    let g = groups(&refs);
    let names: Vec<&str> = g.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        names,
        ["standup", "dri"],
        "largest first, singletons dropped"
    );
    let members: Vec<&str> = g[0].1.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(
        members,
        ["a", "b", "c"],
        "members keep the order they came in"
    );
}

#[test]
fn in_group_matches_the_tag_whatever_case_or_hash_you_type() {
    let items = [tagged("a", &["standup"]), tagged("b", &["dri"])];
    assert_eq!(in_group(&items, "#Standup").len(), 1);
    assert!(in_group(&items, "nope").is_empty());
}

#[test]
fn the_link_you_reach_for_most_is_the_one_on_top() {
    let items = vec![
        link("rare", 1, Some("2026-09-03")),
        link("daily", 20, Some("2026-09-03")),
    ];
    let ranked = ranked(&items, None, "2026-09-03");
    assert_eq!(ranked[0].name, "daily");
}

#[test]
fn a_link_used_today_outranks_a_busier_one_gone_cold() {
    // 3 uses today (3 * 16 = 48) beats 20 uses last quarter (20 * 1 = 20).
    let items = vec![
        link("stale", 20, Some("2026-01-05")),
        link("fresh", 3, Some("2026-09-03")),
    ];
    let ranked = ranked(&items, None, "2026-09-03");
    assert_eq!(ranked[0].name, "fresh");
}

#[test]
fn the_repo_you_are_standing_in_sorts_first_even_when_it_is_used_less() {
    let mut mine = link("mine", 1, Some("2026-01-01"));
    mine.repo = Some("bn".to_string());
    let items = vec![link("busy", 99, Some("2026-09-03")), mine];
    let ranked = ranked(&items, Some("bn"), "2026-09-03");
    assert_eq!(ranked[0].name, "mine");
    assert_eq!(ranked[1].name, "busy");
}

#[test]
fn a_never_opened_link_still_ranks_by_the_day_it_was_saved() {
    let saved_today = Link {
        created: "2026-09-03".to_string(),
        ..link("new", 0, None)
    };
    let saved_last_year = Link {
        created: "2025-09-03".to_string(),
        ..link("old", 0, None)
    };
    let items = vec![saved_last_year, saved_today];
    let ranked = ranked(&items, None, "2026-09-03");
    assert_eq!(ranked[0].name, "new");
}

#[test]
fn the_store_survives_a_round_trip_through_yaml() {
    let items = vec![Link {
        tags: vec!["dri".into(), "telemetry".into()],
        repo: Some("BigMono".into()),
        ..link("pbi", 4, Some("2026-09-02"))
    }];
    let front = LinksFront {
        items: items.clone(),
    };
    let text = format!(
        "---\n{}---\n\n# Links\n",
        serde_yaml::to_string(&front).unwrap()
    );
    let back = crate::store::parse_frontmatter::<LinksFront>(&text).unwrap();
    assert_eq!(back.items, items);
}

#[test]
fn resolve_finds_a_link_by_a_fragment_of_its_url() {
    let items = vec![link("Prod subscription", 0, None)];
    assert_eq!(
        resolve(&items, "example.com/Prod").unwrap().name,
        "Prod subscription"
    );
}

#[test]
fn resolve_refuses_to_guess_between_two_matches() {
    let items = vec![link("portal one", 0, None), link("portal two", 0, None)];
    let err = resolve(&items, "portal").unwrap_err();
    assert!(
        matches!(err, LinkError::Ambiguous(ref c) if c.len() == 2),
        "{err:?}"
    );
}

#[test]
fn an_exact_id_wins_over_any_name_match() {
    let mut decoy = link("k1", 0, None);
    decoy.id = "k9".to_string();
    let mut wanted = link("something else", 0, None);
    wanted.id = "k1".to_string();
    let items = vec![decoy, wanted];
    assert_eq!(resolve(&items, "k1").unwrap().name, "something else");
}

#[test]
fn only_http_urls_are_storable() {
    assert!(valid_url("https://example.com/x"));
    assert!(valid_url("http://example.com"));
    assert!(!valid_url("javascript:alert(1)"));
    assert!(!valid_url("file:///etc/passwd"));
    assert!(!valid_url("data:text/html,<script>"));
    assert!(!valid_url("example.com"));
    assert!(!valid_url("https://exa mple.com"));
}

#[test]
fn a_missing_name_becomes_the_host_and_the_last_path_segment() {
    assert_eq!(
        default_name("https://dev.azure.com/contoso/Project/_workitems/edit/11047593"),
        "dev.azure.com/11047593"
    );
    assert_eq!(default_name("https://example.com"), "example.com");
    assert_eq!(default_name("https://example.com/"), "example.com");
    assert_eq!(
        default_name("https://example.com/docs?q=1"),
        "example.com/docs"
    );
}

/// A config whose `bare_dir()` is `<root>/bare`, holding one bare clone with `remote` as its
/// origin — all `forge::links_for` reads.
fn cfg_with_clone(root: &std::path::Path, repo: &str, remote: &str) -> crate::resolver::Config {
    let bare = root.join("bare").join(format!("{repo}.git"));
    std::fs::create_dir_all(&bare).unwrap();
    std::fs::write(
        bare.join("config"),
        format!("[remote \"origin\"]\n\turl = {remote}\n"),
    )
    .unwrap();
    crate::test_support::config_with_roots(root, root, root.join("worktrees"), "testhost")
}

#[test]
fn a_bare_work_item_id_is_captured_as_its_forge_url() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = cfg_with_clone(
        tmp.path(),
        "bigmono",
        "https://dev.azure.com/contoso/Project/_git/BigMono",
    );
    let got = capturable(
        &cfg,
        "bigmono",
        Some("https://github.com/eduuh/bn/pull/1250"),
        Some("11047593"),
    );
    assert_eq!(
        got,
        vec![
            Capturable {
                label: "PR".into(),
                url: "https://github.com/eduuh/bn/pull/1250".into()
            },
            Capturable {
                label: "work item".into(),
                url: "https://dev.azure.com/contoso/Project/_workitems/edit/11047593".into()
            },
        ]
    );
}

#[test]
fn an_id_no_forge_can_place_is_dropped_rather_than_stored_broken() {
    let tmp = tempfile::tempdir().unwrap();
    // No bare clone for this repo at all — nothing to derive from.
    let cfg =
        crate::test_support::config_with_roots(tmp.path(), tmp.path(), tmp.path(), "testhost");
    assert!(capturable(&cfg, "nope", None, Some("11047593")).is_empty());
}

fn empty_cfg(root: &std::path::Path) -> crate::resolver::Config {
    crate::test_support::config_with_roots(root, root, root, "testhost")
}

#[test]
fn capture_saves_once_and_never_clobbers_a_hand_written_name() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = empty_cfg(tmp.path());
    let pr = "https://github.com/eduuh/bn/pull/1469";
    let saved = capture(&cfg, "bn", "links", Some(pr), None, "2026-10-01").unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].name, "bn links PR");
    rename(&cfg, &saved[0].id, "link groups PR").unwrap();
    let again = capture(&cfg, "bn", "links", Some(pr), None, "2026-10-02").unwrap();
    assert!(again.is_empty(), "already stored, nothing new");
    assert_eq!(read(&cfg)[0].name, "link groups PR");
}

#[test]
fn the_note_lists_its_own_branch_links_before_the_rest_of_the_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = empty_cfg(tmp.path());
    let d = "2026-10-07";
    add(
        &cfg,
        "https://a.example/busy",
        Some("busy"),
        &[],
        Some("bn"),
        Some("main"),
        d,
    )
    .unwrap();
    touch(&cfg, "k1", d).unwrap();
    add(
        &cfg,
        "https://a.example/mine",
        Some("mine"),
        &[],
        Some("bn"),
        Some("feat"),
        d,
    )
    .unwrap();
    add(
        &cfg,
        "https://a.example/other",
        Some("other"),
        &[],
        Some("bigmono"),
        Some("feat"),
        d,
    )
    .unwrap();
    let names: Vec<String> = for_note(&cfg, "bn", "feat", d, 5)
        .into_iter()
        .map(|l| l.name)
        .collect();
    assert_eq!(names, ["mine", "busy"], "branch first, other repos never");
}

#[test]
fn a_session_keeps_its_prs_and_work_items_and_drops_the_rest() {
    let text = "Opened https://github.com/eduuh/bn/pull/1470. See (https://docs.rs/serde) and \
                https://dev.azure.com/o/p/_workitems/edit/123, also \
                `https://github.com/eduuh/bn/pull/1470` again and https://github.com/eduuh/bn";
    assert_eq!(
        notable_urls(text),
        [
            "https://github.com/eduuh/bn/pull/1470",
            "https://dev.azure.com/o/p/_workitems/edit/123",
        ]
    );
}

#[test]
fn harvest_saves_new_urls_only() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = empty_cfg(tmp.path());
    let d = "2026-10-07";
    let pr = "https://github.com/eduuh/bn/pull/1470".to_string();
    add(&cfg, &pr, Some("mine"), &[], None, None, d).unwrap();
    let wi = "https://dev.azure.com/o/p/_workitems/edit/9".to_string();
    let saved = harvest(&cfg, &[pr, wi.clone()], Some("bn"), Some("feat"), d).unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].url, wi);
    assert_eq!(saved[0].branch.as_deref(), Some("feat"));
    assert_eq!(read(&cfg)[0].name, "mine");
}

#[test]
fn a_template_names_each_placeholder_once_and_fills_them_encoded() {
    let url = "https://x.example/{org}/items/{id}?q={id}";
    assert_eq!(placeholders(url), ["org", "id"]);
    let filled = fill(
        url,
        &[("org".into(), "a b".into()), ("id".into(), "42".into())],
    );
    assert_eq!(filled, "https://x.example/a%20b/items/42?q=42");
    assert!(placeholders("https://x.example/plain").is_empty());
    assert!(valid_url(url), "a template is still a storable URL");
}

#[test]
fn re_adding_without_a_name_keeps_the_name() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = empty_cfg(tmp.path());
    let url = "https://example.com/board";
    add(&cfg, url, Some("Team board"), &[], None, None, "2026-10-01").unwrap();
    let l = add(
        &cfg,
        url,
        None,
        &["standup".into()],
        None,
        None,
        "2026-10-01",
    )
    .unwrap();
    assert_eq!(l.name, "Team board");
    assert_eq!(l.tags, ["standup"]);
}

#[test]
fn retag_adds_and_drops_without_touching_other_tags() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = empty_cfg(tmp.path());
    let l = add(
        &cfg,
        "https://example.com/x",
        None,
        &["dri".into(), "docs".into()],
        None,
        None,
        "2026-10-01",
    )
    .unwrap();
    let l = retag(
        &cfg,
        &l.id,
        &["Standup".into(), "dri".into()],
        &["docs".into()],
    )
    .unwrap();
    assert_eq!(l.tags, ["dri", "standup"]);
}

#[test]
fn a_note_with_neither_owes_the_store_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg =
        crate::test_support::config_with_roots(tmp.path(), tmp.path(), tmp.path(), "testhost");
    assert!(capturable(&cfg, "bn", None, None).is_empty());
}

#[test]
fn an_unlisted_tag_is_reported_but_still_kept() {
    let vocab = vec!["dri".to_string(), "telemetry".to_string()];
    let (accepted, unknown) = split_tags("DRI, sprawl, telemetry", &vocab);
    assert_eq!(accepted, vec!["dri", "sprawl", "telemetry"]);
    assert_eq!(unknown, vec!["sprawl"]);
}

#[test]
fn tags_are_lowercased_and_deduplicated() {
    let (accepted, _) = split_tags("Security, security, , SECURITY", &[]);
    assert_eq!(accepted, vec!["security"]);
}

#[test]
fn an_expired_link_drops_out_of_every_listing_on_its_date() {
    let mut l = link("bridge", 5, Some("2026-10-07"));
    l.expires = Some("2026-10-08".into());
    let items = vec![l, link("board", 1, None)];
    let names = |today: &str| -> Vec<String> {
        ranked(&items, None, today)
            .iter()
            .map(|l| l.name.clone())
            .collect()
    };
    assert_eq!(names("2026-10-07"), ["bridge", "board"]);
    assert_eq!(
        names("2026-10-08"),
        ["board"],
        "hidden from its expiry date on"
    );
}

#[test]
fn a_store_written_before_new_and_expires_still_reads() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = empty_cfg(tmp.path());
    std::fs::create_dir_all(cfg.links_file().parent().unwrap()).unwrap();
    std::fs::write(
        cfg.links_file(),
        "---\nitems:\n- id: k1\n  url: https://example.com/a\n  name: a\n  created: 2026-01-01\n  uses: 0\n---\n",
    )
    .unwrap();
    let got = read(&cfg);
    assert_eq!(got.len(), 1);
    assert!(!got[0].new && got[0].expires.is_none());
    write(&cfg, &got).unwrap();
    let text = std::fs::read_to_string(cfg.links_file()).unwrap();
    assert!(
        !text.contains("new:") && !text.contains("expires:"),
        "unset fields stay out: {text}"
    );
}

#[test]
fn only_hook_saved_links_wait_for_review_and_keeping_clears_them() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = empty_cfg(tmp.path());
    let d = "2026-10-07";
    add(&cfg, "https://example.com/chosen", None, &[], None, None, d).unwrap();
    let pr = "https://github.com/o/r/pull/7".to_string();
    let saved = harvest(&cfg, &[pr], Some("bn"), Some("feat"), d).unwrap();
    assert!(saved[0].new);
    let items = read(&cfg);
    let waiting: Vec<&str> = pending(&items, d).iter().map(|l| l.url.as_str()).collect();
    assert_eq!(waiting, ["https://github.com/o/r/pull/7"]);
    assert_eq!(mark_reviewed(&cfg, &[saved[0].id.clone()]).unwrap(), 1);
    assert!(pending(&read(&cfg), d).is_empty());
}

#[test]
fn a_ttl_reads_days_and_weeks_and_refuses_zero() {
    assert_eq!(parse_ttl("7d"), Some(7));
    assert_eq!(parse_ttl("2W"), Some(14));
    assert_eq!(parse_ttl("30"), Some(30));
    assert_eq!(parse_ttl("0d"), None);
    assert_eq!(parse_ttl("soon"), None);
}

#[test]
fn prune_deletes_only_what_has_expired() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = empty_cfg(tmp.path());
    let d = "2026-10-07";
    let bridge = add(&cfg, "https://example.com/bridge", None, &[], None, None, d).unwrap();
    add(&cfg, "https://example.com/board", None, &[], None, None, d).unwrap();
    let l = set_expiry(&cfg, &bridge.id, d, 7).unwrap();
    assert_eq!(l.expires.as_deref(), Some("2026-10-14"));
    assert!(prune(&cfg, "2026-10-13").unwrap().is_empty());
    let gone = prune(&cfg, "2026-10-14").unwrap();
    assert_eq!(gone.len(), 1);
    assert_eq!(read(&cfg).len(), 1);
}
