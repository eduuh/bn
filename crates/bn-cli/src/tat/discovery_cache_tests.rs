//! The discovery cache's freshness and round-trip rules.
//!
//! Out of line from `tat.rs`, which sits on the 1000-line ratchet's grandfathered list and may not
//! gain a line: the rule is that a file needing to grow is split, and a test module is the half
//! that moves without touching behaviour. `#[path]` keeps it a child of `tat` so it still reaches
//! the private items it asserts on.
use super::codespaces::{cs_cache_fresh, cs_raw};
use super::discovery::{
    discovery_cache_fresh, discovery_cache_path, read_discovery_cache, scan_worktrees,
    write_discovery_cache, CachedRemotePrs, DiscoveryCache,
};
use super::*;

fn roots(root: &Path) -> Roots {
    Roots {
        primary: root.to_path_buf(),
        primary_bare: root.join("bare"),
        primary_worktree: root.join("worktree"),
        extras: Vec::new(),
        win_scoped: false,
    }
}

fn add_bare(root: &Path, repo: &str) {
    std::fs::create_dir_all(root.join("bare").join(format!("{repo}.git"))).unwrap();
}

#[test]
fn ordered_refs_cache_hits_until_refresh_and_isolates_roots() {
    let _g = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache");
    let _cache = bn_core::test_support::EnvGuard::set("XDG_CACHE_HOME", &cache);
    let _ttl = bn_core::test_support::EnvGuard::set("WF_TAT_TTL", "30");
    let _now = bn_core::test_support::EnvGuard::set("BN_NOW", "100");
    // Pin zoxide at an empty data dir. Without it the ordering below depends on whether the
    // machine running the test happens to have a zoxide db: with one, every ref scores 0.0 and
    // `frecency_order`'s descending tiebreak reverses the list; with none, `sort_by_frecency`
    // returns the ascending walk untouched. That ambient split is what reddened the release
    // gate — green on a workstation, red on a CI runner that has never seen zoxide.
    let _zoxide = bn_core::test_support::EnvGuard::set("_ZO_DATA_DIR", tmp.path().join("zoxide"));
    let first = tmp.path().join("first");
    let second = tmp.path().join("second");
    add_bare(&first, "alpha");
    add_bare(&second, "other");

    assert!(discovery_cache_fresh(70));
    assert!(!discovery_cache_fresh(69));
    assert_eq!(ordered_refs(&roots(&first), true), vec!["alpha"]);
    add_bare(&first, "beta");
    assert_eq!(
        ordered_refs(&roots(&first), false),
        vec!["alpha"],
        "fresh cache should avoid another filesystem walk"
    );
    assert_eq!(
        ordered_refs(&roots(&first), true),
        vec!["alpha", "beta"],
        "forced refresh should expose new repositories"
    );
    assert_eq!(
        ordered_refs(&roots(&second), false),
        vec!["other"],
        "a different root must not reuse the first root's snapshot"
    );
}

#[test]
fn worktree_cache_refreshes_and_corrupt_cache_is_a_miss() {
    let _g = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache");
    let _cache = bn_core::test_support::EnvGuard::set("XDG_CACHE_HOME", &cache);
    let root = tmp.path().join("root");
    let repo_dir = root.join("worktree").join("alpha");
    std::fs::create_dir_all(repo_dir.join("one")).unwrap();
    let roots = roots(&root);

    assert_eq!(scan_worktrees(&roots, "alpha", true), vec!["one"]);
    std::fs::create_dir_all(repo_dir.join("two")).unwrap();
    assert_eq!(scan_worktrees(&roots, "alpha", false), vec!["one"]);
    assert_eq!(scan_worktrees(&roots, "alpha", true), vec!["one", "two"]);

    // The main worktree is never a row — picking "main" opens the main slot instead.
    std::fs::create_dir_all(repo_dir.join("main")).unwrap();
    std::fs::create_dir_all(repo_dir.join("_pool").join("1")).unwrap();
    assert_eq!(scan_worktrees(&roots, "alpha", true), vec!["one", "two"]);

    std::fs::write(
        discovery_cache_path(),
        r#"{"roots_key":"legacy","refs_saved_at":1,"refs":["alpha"],"worktrees":{}}"#,
    )
    .unwrap();
    let legacy = read_discovery_cache();
    assert_eq!(legacy.refs, vec!["alpha"]);
    assert!(legacy.remote_prs.is_empty());

    std::fs::write(discovery_cache_path(), "not json").unwrap();
    assert!(read_discovery_cache().refs.is_empty());
    assert!(read_discovery_cache().worktrees.is_empty());
}

#[test]
fn remote_pr_discovery_reuses_a_fresh_per_repo_cache_entry() {
    let _g = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache");
    let _cache = bn_core::test_support::EnvGuard::set("XDG_CACHE_HOME", &cache);
    let _now = bn_core::test_support::EnvGuard::set("BN_NOW", "100");
    let root = tmp.path().join("root");
    add_bare(&root, "alpha");
    let roots = roots(&root);
    let bare = root.join("bare/alpha.git").to_string_lossy().to_string();
    let expected = wt::RemotePr {
        number: 42,
        title: "Cross-laptop work".into(),
        branch: "feat/remote".into(),
        url: "https://example.test/42".into(),
    };
    let mut discovery = DiscoveryCache::default();
    discovery.remote_prs.insert(
        bare,
        CachedRemotePrs {
            saved_at: 100,
            prs: vec![expected.clone()],
        },
    );
    write_discovery_cache(&discovery);

    assert_eq!(remote_prs(&roots, "alpha"), vec![expected]);
}

#[test]
fn remote_pr_discovery_serves_a_stale_entry_rather_than_blocking_on_the_forge() {
    let _g = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache");
    let _cache = bn_core::test_support::EnvGuard::set("XDG_CACHE_HOME", &cache);
    let _ttl = bn_core::test_support::EnvGuard::set("WF_TAT_TTL", "30");
    // Far past the TTL: the old code shelled out to the forge here, which is the several
    // seconds the picker used to spend before painting a row.
    let _now = bn_core::test_support::EnvGuard::set("BN_NOW", "10000");
    let _warm = bn_core::test_support::EnvGuard::set("BN_TAT_WARM", "0");
    let root = tmp.path().join("root");
    add_bare(&root, "alpha");
    let roots = roots(&root);
    let bare = root.join("bare/alpha.git").to_string_lossy().to_string();
    let stale = wt::RemotePr {
        number: 7,
        title: "Stale but useful".into(),
        branch: "feat/stale".into(),
        url: "https://example.test/7".into(),
    };
    let mut discovery = DiscoveryCache::default();
    discovery.remote_prs.insert(
        bare,
        CachedRemotePrs {
            saved_at: 100,
            prs: vec![stale.clone()],
        },
    );
    write_discovery_cache(&discovery);

    assert_eq!(
        remote_prs(&roots, "alpha"),
        vec![stale],
        "a stale entry is still the answer the picker paints"
    );
}

#[test]
fn codespace_rows_come_from_the_cache_even_when_it_is_cold() {
    let _g = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let _cache = bn_core::test_support::EnvGuard::set("XDG_CACHE_HOME", tmp.path());
    let _warm = bn_core::test_support::EnvGuard::set("BN_TAT_WARM", "0");

    // No cache file at all: an empty array, not a `gh cs list` the person waits on.
    assert_eq!(cs_raw(), "[]");
    assert!(!cs_cache_fresh());
}
