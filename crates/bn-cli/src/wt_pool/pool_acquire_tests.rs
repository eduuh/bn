//! Pool **acquire / lease** integration tests — the half of the git-backed suite that exercises
//! `acquire_slot` and the lease ledger rather than raw slot mechanics. Split out of
//! `git_integration_tests` when that file crossed the 1000-line ratchet; the helpers and the
//! reasoning about `env_lock` live there and are shared from here.

use super::git_integration_tests::{build_repo_into, git, head_ref, mk_bare};
use super::*;

/// Regression for the warm-hit-on-a-dead-checkout bug: a lease can outlive its slot dir (removed
/// out-of-band while the holder pid is still alive, so `reclaim_stale` keeps the lease). The
/// idempotent "warm hit" must rebuild the checkout, never hand back the missing path.
#[test]
fn acquire_repairs_a_lease_whose_checkout_vanished() {
    let _env = bn_core::test_support::env_lock();
    let (tmp, bare) = mk_bare();
    // Stub occupancy so the slot scans never reach for tmux / the agent registry.
    TEST_OCCUPANTS.with(|c| *c.borrow_mut() = Some(BTreeMap::new()));

    let wt_root = tmp.path().join("wt");
    let pdir = wt_root.join("bn").join("_pool");
    std::fs::create_dir_all(&pdir).unwrap();

    // A live lease for slot 2 → "feat", but its checkout was never created (vanished dir).
    let mut leases = Leases::default();
    leases.slots.insert(
        2,
        Lease {
            branch: "feat".into(),
            holder: "me".into(),
            pid: std::process::id(), // alive → not reclaimed as a dead holder
            at: now_epoch(),
            boot_id: String::new(), // unknown boot → liveness falls back to the (alive) pid
            comparison_slot: None,
        },
    );
    write_leases(&pdir, &leases).unwrap();

    let ctx = PoolCtx {
        bare: PathBuf::from(&bare),
        repo: "bn".into(),
        pool_dir: pdir.clone(),
        worktree_dir: wt_root,
        size: 4,
    };

    let leased = acquire_slot(&ctx, "feat", None, None, SlotRole::Worker, false)
        .expect("a vanished checkout must be rebuilt, not surfaced as a dead warm hit");
    // Same branch's tenancy predates this call, so it is not a fresh lease…
    assert!(!leased.fresh);
    // …but the returned slot is now a real, live checkout on the right branch.
    assert_eq!(leased.slot, slot_path(&pdir, 2));
    assert!(is_live_slot(&leased.slot));
    assert_eq!(head_ref(&leased.slot), "feat");
}

/// Regression: a full pool of leases whose holders never die must not read as full.
///
/// Every worker slot is leased, no agent is in any of them, and each lease records a holder pid
/// that is very much alive — the real-world shape, because the recorded holder is the caller's
/// parent: a tmux server (every popup command's parent) or a login shell. The dead-holder sweep
/// frees nothing there, so `acquire_slot` used to report "Pool full" and `bn dispatch` spilled
/// into a cold `worktree/<repo>/<branch>` checkout while warm, empty slots sat idle.
#[test]
fn acquire_reclaims_leaked_leases_before_declaring_the_pool_full() {
    let _env = bn_core::test_support::env_lock();
    let (tmp, bare) = mk_bare();
    TEST_OCCUPANTS.with(|c| *c.borrow_mut() = Some(BTreeMap::new()));

    let wt_root = tmp.path().join("wt");
    let pdir = wt_root.join("bn").join("_pool");
    std::fs::create_dir_all(&pdir).unwrap();

    // Fill every worker slot (2..=4) with an aged lease held by this very process.
    let mut leases = Leases::default();
    for n in 2..=4 {
        let slot = slot_path(&pdir, n);
        assert!(add_detached_slot(&bare, &slot.to_string_lossy()));
        leases.slots.insert(
            n,
            Lease {
                branch: format!("gone-{n}"),
                holder: "tmux".into(),
                pid: std::process::id(), // immortal holder: alive for the whole test
                at: now_epoch() - LEAKED_LEASE_GRACE_SECS - 1,
                boot_id: String::new(),
                comparison_slot: None,
            },
        );
    }
    write_leases(&pdir, &leases).unwrap();

    let ctx = PoolCtx {
        bare: PathBuf::from(&bare),
        repo: "bn".into(),
        pool_dir: pdir.clone(),
        worktree_dir: wt_root,
        size: 4,
    };

    let leased = acquire_slot(&ctx, "feat", None, None, SlotRole::Worker, false)
        .expect("a pool of leases nobody is using must yield a slot, not read as full");
    assert!(leased.fresh);
    assert!(is_live_slot(&leased.slot));
    assert_eq!(head_ref(&leased.slot), "feat");
    // The ledger names the new tenant, in exactly the slot it handed out.
    let after = read_leases(&pdir);
    let feat: Vec<u32> = after
        .slots
        .iter()
        .filter(|(_, l)| l.branch == "feat")
        .map(|(n, _)| *n)
        .collect();
    assert_eq!(feat.len(), 1, "one lease for the branch, not two");
    assert_eq!(leased.slot, slot_path(&pdir, feat[0]));
}

/// A lease held by a live *agent* is never reclaimed, however old it is — the guard that keeps
/// the sweep above from switching a branch out from under somebody. With demand-driven growth the
/// pool now steps *past* those tenants into a new slot instead of refusing, which is the same
/// promise from the occupant's side: nobody is evicted either way.
#[test]
fn acquire_grows_past_occupied_slots_rather_than_reclaiming_one() {
    let _env = bn_core::test_support::env_lock();
    let (tmp, bare) = mk_bare();

    let wt_root = tmp.path().join("wt");
    let pdir = wt_root.join("bn").join("_pool");
    std::fs::create_dir_all(&pdir).unwrap();

    let mut leases = Leases::default();
    let mut occupants = BTreeMap::new();
    for n in 2..=4 {
        let slot = slot_path(&pdir, n);
        assert!(add_detached_slot(&bare, &slot.to_string_lossy()));
        let branch = format!("busy-{n}");
        occupants.insert(
            slot.clone(),
            vec![crate::agent_lease::Occupant {
                repo: Some("bn".into()),
                branch: Some(branch.clone()),
                session_id: Some(format!("session-{n}")),
                tmux_target: format!("bn:{branch}"),
                kind: "dispatch".into(),
                since: now_epoch() - LEAKED_LEASE_GRACE_SECS - 1,
            }],
        );
        leases.slots.insert(
            n,
            Lease {
                branch,
                holder: "tmux".into(),
                pid: std::process::id(),
                at: now_epoch() - LEAKED_LEASE_GRACE_SECS - 1,
                boot_id: String::new(),
                comparison_slot: None,
            },
        );
    }
    write_leases(&pdir, &leases).unwrap();
    TEST_OCCUPANTS.with(|c| *c.borrow_mut() = Some(occupants));

    let ctx = PoolCtx {
        bare: PathBuf::from(&bare),
        repo: "bn".into(),
        pool_dir: pdir.clone(),
        worktree_dir: wt_root,
        size: 4,
    };

    let leased = acquire_slot(&ctx, "feat", None, None, SlotRole::Worker, false)
        .expect("a pool whose every slot is busy grows rather than refusing");
    assert_eq!(
        leased.slot,
        slot_path(&pdir, 5),
        "growth adds the next slot, it does not recycle a busy one"
    );
    // Every original tenancy survives untouched.
    let after = read_leases(&pdir);
    for n in 2..=4 {
        assert_eq!(
            after.slots.get(&n).map(|l| l.branch.as_str()),
            Some(format!("busy-{n}").as_str())
        );
    }
}

/// The happy warm hit is untouched: an existing live checkout for the branch is handed straight
/// back (`fresh: false`) without a rebuild.
#[test]
fn acquire_warm_hit_reuses_a_live_checkout() {
    let _env = bn_core::test_support::env_lock();
    let (tmp, bare) = mk_bare();
    TEST_OCCUPANTS.with(|c| *c.borrow_mut() = Some(BTreeMap::new()));

    let wt_root = tmp.path().join("wt");
    let pdir = wt_root.join("bn").join("_pool");
    std::fs::create_dir_all(&pdir).unwrap();

    // Pre-build slot 2 as a live checkout on feat, and record its lease.
    let slot = slot_path(&pdir, 2);
    assert!(add_detached_slot(&bare, &slot.to_string_lossy()));
    assert!(switch_into_slot_from(
        &slot.to_string_lossy(),
        &bare,
        "feat",
        None
    ));
    let mut leases = Leases::default();
    leases.slots.insert(
        2,
        Lease {
            branch: "feat".into(),
            holder: "me".into(),
            pid: std::process::id(),
            at: now_epoch(),
            boot_id: String::new(),
            comparison_slot: None,
        },
    );
    write_leases(&pdir, &leases).unwrap();

    let ctx = PoolCtx {
        bare: PathBuf::from(&bare),
        repo: "bn".into(),
        pool_dir: pdir,
        worktree_dir: wt_root,
        size: 4,
    };
    let leased = acquire_slot(&ctx, "feat", None, None, SlotRole::Worker, false).expect("warm hit");
    assert!(!leased.fresh);
    assert_eq!(leased.slot, slot);
    assert!(is_live_slot(&leased.slot));
    assert_eq!(head_ref(&leased.slot), "feat");
}

#[test]
fn acquire_dispatch_branch_starts_from_requested_base() {
    let _env = bn_core::test_support::env_lock();
    let (tmp, bare) = mk_bare();
    TEST_OCCUPANTS.with(|c| *c.borrow_mut() = Some(BTreeMap::new()));

    let base_wt = tmp.path().join("base-worktree");
    let base_wt_s = base_wt.to_string_lossy().to_string();
    assert!(git(&[
        "--git-dir",
        &bare,
        "worktree",
        "add",
        "-b",
        "agent/forge-agent-cli",
        &base_wt_s,
        "main",
    ]));
    std::fs::write(base_wt.join("forge-skill.md"), "rich markdown skill\n").unwrap();
    assert!(git(&["-C", &base_wt_s, "add", "forge-skill.md"]));
    assert!(git(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "-C",
        &base_wt_s,
        "commit",
        "-qm",
        "add forge skill"
    ]));
    assert!(git(&[
        "--git-dir",
        &bare,
        "worktree",
        "remove",
        "--force",
        &base_wt_s
    ]));

    let wt_root = tmp.path().join("wt");
    let pdir = wt_root.join("bn").join("_pool");
    std::fs::create_dir_all(&pdir).unwrap();
    let slot = slot_path(&pdir, 2);
    assert!(add_detached_slot(&bare, &slot.to_string_lossy()));

    let ctx = PoolCtx {
        bare: PathBuf::from(&bare),
        repo: "bn".into(),
        pool_dir: pdir,
        worktree_dir: wt_root,
        size: 4,
    };
    let leased = acquire_slot(
        &ctx,
        "dispatch/forge-smoke",
        Some("agent/forge-agent-cli"),
        None,
        SlotRole::Worker,
        false,
    )
    .expect("dispatch lease");

    assert_eq!(leased.slot, slot);
    assert_eq!(head_ref(&leased.slot), "dispatch/forge-smoke");
    assert_eq!(
        std::fs::read_to_string(leased.slot.join("forge-skill.md")).unwrap(),
        "rich markdown skill\n"
    );
}

/// A lease lands on the slot whose build completed, not the lowest-numbered checkout. In a
/// monorepo pool the lowest idle slot was exactly the one warm kept skipping (dirty, or its
/// install had failed), so every "warm hit" still paid the full install. The stamp is cleared when
/// the checkout is recreated: a fresh `worktree add` has no deps whatever the slot number had.
#[test]
fn acquire_prefers_a_built_slot_over_a_lower_unbuilt_one() {
    let _env = bn_core::test_support::env_lock();
    let (tmp, bare) = mk_bare();
    TEST_OCCUPANTS.with(|c| *c.borrow_mut() = Some(BTreeMap::new()));

    let wt_root = tmp.path().join("wt");
    let pdir = wt_root.join("bn").join("_pool");
    std::fs::create_dir_all(&pdir).unwrap();

    // Slots 2 and 3 are both live checkouts; only 3 has a completed build behind it.
    for n in [2, 3] {
        assert!(add_detached_slot(
            &bare,
            &slot_path(&pdir, n).to_string_lossy()
        ));
    }
    write_warm_stamp(&pdir, 3, "abc123");
    assert_eq!(
        read_warm_stamps(&pdir),
        [3u32].into_iter().collect::<BTreeSet<u32>>()
    );

    let ctx = PoolCtx {
        bare: PathBuf::from(&bare),
        repo: "bn".into(),
        pool_dir: pdir.clone(),
        worktree_dir: wt_root,
        size: 4,
    };
    let leased = acquire_slot(&ctx, "feat", None, None, SlotRole::Worker, false).expect("lease");
    assert!(leased.fresh);
    assert_eq!(leased.slot, slot_path(&pdir, 3));
    assert_eq!(head_ref(&leased.slot), "feat");

    // Recreating a checkout forgets its build.
    assert!(add_detached_slot(
        &bare,
        &slot_path(&pdir, 4).to_string_lossy()
    ));
    write_warm_stamp(&pdir, 4, "abc123");
    force_remove_worktree(&bare, &slot_path(&pdir, 4));
    assert!(add_detached_slot(
        &bare,
        &slot_path(&pdir, 4).to_string_lossy()
    ));
    assert!(!read_warm_stamps(&pdir).contains(&4));
}

/// Regression for the `--new` warm no-op: `pool_warm(--new)` must rebuild a *broken* stub
/// (a dir with no `.git`, left by an interrupted create), not skip it as "existing". The old
/// guard keyed off directory presence, so `warm --new` left a broken slot broken and the next
/// dispatch paid a cold create.
#[test]
fn pool_warm_new_rebuilds_a_broken_stub() {
    let _g = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Stage the `$WT_ROOT` layout `wt::layout` expects: `<root>/bare/bn.git`, worktrees under
    // `<root>/worktree`.
    let bare = root.join("bare").join("bn.git");
    build_repo_into(&root.join("bn-src"), &bare);

    let _wt_root = bn_core::test_support::EnvGuard::set("WT_ROOT", root);
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", root.join("state"));
    // Never reach for a real tmux/agent registry during the slot scan.
    TEST_OCCUPANTS.with(|c| *c.borrow_mut() = Some(BTreeMap::new()));

    // A broken stub for slot 2: the dir exists but there's no `.git`, so it's not a live
    // checkout — exactly the interrupted-create state the old `--new` guard skipped.
    let pool_dir = root.join("worktree").join("bn").join("_pool");
    std::fs::create_dir_all(pool_dir.join("2")).unwrap();
    std::fs::write(pool_dir.join("2").join("stray.txt"), "junk\n").unwrap();
    assert!(!is_live_slot(&pool_dir.join("2")));

    let cfg = bn_core::test_support::config(root);
    // here = true (stay inline, no tmux window), new = true.
    assert!(pool_warm(&cfg, Some("bn".to_string()), true, true, None));

    // The broken stub is now a real, live checkout — the whole point of the fix.
    assert!(is_live_slot(&pool_dir.join("2")));
}

#[test]
fn pool_warm_single_slot_only_touches_that_slot() {
    let _g = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    let bare = root.join("bare").join("bn.git");
    build_repo_into(&root.join("bn-src"), &bare);

    let _wt_root = bn_core::test_support::EnvGuard::set("WT_ROOT", root);
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", root.join("state"));
    TEST_OCCUPANTS.with(|c| *c.borrow_mut() = Some(BTreeMap::new()));

    let cfg = bn_core::test_support::config(root);
    assert!(pool_warm(
        &cfg,
        Some("bn".to_string()),
        true,
        false,
        Some(2)
    ));

    let pool_dir = root.join("worktree").join("bn").join("_pool");
    assert!(is_live_slot(&pool_dir.join("2")));
    assert!(!pool_dir.join("1").exists(), "slot 1 should stay untouched");
    assert!(!pool_dir.join("3").exists(), "slot 3 should stay untouched");
    assert!(!pool_dir.join("4").exists(), "slot 4 should stay untouched");
}

/// Regression for the `pool-warm` cron failing on a pool `bn dispatch` created implicitly.
///
/// The cron must warm a pool for a repo nobody registered — which, with the pools config gone, is
/// every repo. Kept from the era when `resolve_pool` refused an unregistered repo and the warm died
/// exit 1 on a pool sitting right there on disk, re-queuing itself on the next dispatch.
#[test]
fn cron_warms_a_pool_for_a_repo_nobody_registered() {
    let _g = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    let bare = root.join("bare").join("nvim.git");
    build_repo_into(&root.join("nvim-src"), &bare);

    let _wt_root = bn_core::test_support::EnvGuard::set("WT_ROOT", root);
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", root.join("state"));
    let cfg0 = bn_core::test_support::config(root);
    // No pool on disk yet, so it derives to the floor.
    assert_eq!(
        bn_core::pool::pool_size(&cfg0, "nvim"),
        bn_core::pool::DEFAULT_POOL_SIZE
    );
    TEST_OCCUPANTS.with(|c| *c.borrow_mut() = Some(BTreeMap::new()));

    let cfg = bn_core::test_support::config(root);
    let ctx = bn_core::resolver::Resolved {
        repo: "nvim".to_string(),
        branch: "main".to_string(),
    };
    assert_eq!(super::job_pool_warm(&cfg, Some(&ctx)), 0);

    // Not just a green exit — the slots the next dispatch leases are actually there.
    let pool_dir = root.join("worktree").join("nvim").join("_pool");
    for n in 2..=bn_core::pool::DEFAULT_POOL_SIZE as u32 {
        assert!(
            is_live_slot(&slot_path(&pool_dir, n)),
            "slot {n} should hold a live checkout"
        );
    }
}

/// The other half of [`cron_warms_a_pool_dispatch_created_for_an_unregistered_repo`]: a warm can
/// be asked for a clone that has since gone. That is a stale request, not a broken cron, so it
/// reports a skip and exits 0 rather than paging as a cron failure.
#[test]
fn cron_warm_skips_a_repo_with_no_bare_clone() {
    let _g = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("bare")).unwrap();

    let _wt_root = bn_core::test_support::EnvGuard::set("WT_ROOT", root);
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", root.join("state"));

    let cfg = bn_core::test_support::config(root);
    let ctx = bn_core::resolver::Resolved {
        repo: "long-gone".to_string(),
        branch: "main".to_string(),
    };
    assert_eq!(super::job_pool_warm(&cfg, Some(&ctx)), 0);
}

/// The pool reconcile a warm runs first must make a pool's ledger match its slots without a dispatch
/// having to trip over the drift first: adopt the slot a live agent holds unnamed, and hand back
/// the one whose lease outlived everybody.
///
/// This is the regression for a pool sitting at "every worker slot leased" with only a fraction of
/// them actually occupied — the state that made `bn dispatch` spill into cold worktrees.
#[test]
fn reconcile_pools_adopts_occupied_and_returns_empty_slots() {
    let _g = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    let bare = root.join("bare").join("bn.git");
    build_repo_into(&root.join("bn-src"), &bare);

    let _wt_root = bn_core::test_support::EnvGuard::set("WT_ROOT", root);
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", root.join("state"));

    let pool_dir = root.join("worktree").join("bn").join("_pool");
    std::fs::create_dir_all(&pool_dir).unwrap();
    let bare_s = bare.to_string_lossy().to_string();
    // The whole worker range of a floor-sized pool (2..=DEFAULT_POOL_SIZE), because the tick-head
    // sweep only unleases when the pool actually needs the capacity — a free slot means a leaked
    // lease costs nothing and eagerly resetting it could pull a checkout from under a live agent
    // no probe recognized. So the fixture has to leave no free slot.
    for n in 2..=bn_core::pool::DEFAULT_POOL_SIZE as u32 {
        assert!(add_detached_slot(
            &bare_s,
            &slot_path(&pool_dir, n).to_string_lossy()
        ));
    }

    // Slot 2: leased, aged, and nobody in it. Slot 3: no lease at all, but a live agent is working
    // in it — the pair the reconcile has to fix in opposite directions. Slot 4 is honestly busy,
    // and only there so the pool is full.
    let busy = |branch: &str| crate::agent_lease::Occupant {
        repo: Some("bn".into()),
        branch: Some(branch.into()),
        session_id: Some(format!("session-{branch}")),
        tmux_target: format!("bn:{branch}"),
        kind: "dispatch".into(),
        since: now_epoch() - 60,
    };
    let mut leases = Leases::default();
    leases.slots.insert(
        2,
        Lease {
            branch: "gone".into(),
            holder: "tmux".into(),
            pid: std::process::id(), // immortal holder — the pid check would never free this
            at: now_epoch() - LEAKED_LEASE_GRACE_SECS - 1,
            boot_id: String::new(),
            comparison_slot: None,
        },
    );
    leases.slots.insert(
        4,
        Lease {
            branch: "working".into(),
            holder: "tmux".into(),
            pid: std::process::id(),
            at: now_epoch() - LEAKED_LEASE_GRACE_SECS - 1,
            boot_id: String::new(),
            comparison_slot: None,
        },
    );
    write_leases(&pool_dir, &leases).unwrap();
    TEST_OCCUPANTS.with(|c| {
        *c.borrow_mut() = Some(BTreeMap::from([
            (slot_path(&pool_dir, 3), vec![busy("busy")]),
            (slot_path(&pool_dir, 4), vec![busy("working")]),
        ]))
    });

    let cfg = bn_core::test_support::config(root);
    super::reclaim::reconcile_pools(&cfg, now_epoch());

    let after = read_leases(&pool_dir);
    assert!(
        !after.slots.contains_key(&2),
        "a lease nobody is under must be handed back, whatever its holder pid says"
    );
    assert_eq!(
        after.slots.get(&3).map(|l| l.branch.as_str()),
        Some("busy"),
        "a slot a live agent holds must be adopted, not left reading as idle"
    );
    assert_eq!(
        after.slots.get(&4).map(|l| l.branch.as_str()),
        Some("working"),
        "a lease whose agent is genuinely there is left alone"
    );
}

/// The one reclaim path has to be safe to run twice, because a background reclaim can start while
/// an earlier reclaim (or `wt pool return`, or the reaper) is already in flight. An unleased slot is the goal state, so it reports success rather than "refused" — a
/// cleanup log that reports failure for work that is already done is a log nobody reads.
#[test]
fn cleaning_an_already_free_slot_succeeds_instead_of_reporting_a_refusal() {
    let _g = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let bare = root.join("bare").join("bn.git");
    build_repo_into(&root.join("bn-src"), &bare);

    let _wt_root = bn_core::test_support::EnvGuard::set("WT_ROOT", root);
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", root.join("state"));
    TEST_OCCUPANTS.with(|c| *c.borrow_mut() = Some(BTreeMap::new()));

    let pdir = root.join("worktree").join("bn").join("_pool");
    let slot = pdir.join("2");
    std::fs::create_dir_all(&pdir).unwrap();
    assert!(git(&[
        "--git-dir",
        &bare.to_string_lossy(),
        "worktree",
        "add",
        "-q",
        &slot.to_string_lossy(),
        "main"
    ]));
    // No lease written at all — the slot is warm and free.
    assert!(read_leases(&pdir).slots.is_empty());

    let cfg = bn_core::test_support::config(root);
    assert!(
        crate::wt::cleanup_one(&cfg, &slot, &root.join("worktree"), false).is_ok(),
        "an already-free slot needs no reclaiming; that is success"
    );
    assert!(slot.is_dir(), "and the warm checkout is left alone");

    // The human-facing surface keeps its stricter contract: `wt pool return` on an unleased slot
    // still refuses and still points at --force. One core, two deliberate reports.
    assert!(!crate::wt_pool::return_slot(&cfg, &slot, false));
}

/// `--force` on an occupied-but-unleased slot must still be the eviction valve, not a bypass: the
/// unified core checks occupancy for every caller, so forcing past a *missing lease* cannot also
/// force past a live agent that `evict_occupant` refuses to evict.
#[test]
fn forcing_an_unleased_slot_still_refuses_a_live_occupant() {
    let _g = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let bare = root.join("bare").join("bn.git");
    build_repo_into(&root.join("bn-src"), &bare);

    let _wt_root = bn_core::test_support::EnvGuard::set("WT_ROOT", root);
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", root.join("state"));

    let pdir = root.join("worktree").join("bn").join("_pool");
    let slot = pdir.join("2");
    std::fs::create_dir_all(&pdir).unwrap();
    assert!(git(&[
        "--git-dir",
        &bare.to_string_lossy(),
        "worktree",
        "add",
        "-q",
        &slot.to_string_lossy(),
        "main"
    ]));

    // A live occupant with no session id — nothing can prove it finished, so it is never evictable.
    let mut occupants = BTreeMap::new();
    occupants.insert(
        slot.clone(),
        vec![crate::agent_lease::Occupant {
            repo: Some("bn".to_string()),
            branch: None,
            // No session id: nothing on the machine can prove this agent finished, so
            // `fleet_gc::classify` never calls it evictable.
            session_id: None,
            tmux_target: "bn:2".to_string(),
            kind: "claude".to_string(),
            since: 0,
        }],
    );
    TEST_OCCUPANTS.with(|c| *c.borrow_mut() = Some(occupants));

    let cfg = bn_core::test_support::config(root);
    assert!(matches!(
        crate::wt_pool::release_slot(&cfg, &slot, true),
        SlotRelease::Occupied { .. }
    ));
}

#[test]
fn paired_dispatch_leases_and_returns_feature_and_main_slots_together() {
    let _g = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let bare = root.join("bare").join("bn.git");
    build_repo_into(&root.join("bn-src"), &bare);

    let _wt_root = bn_core::test_support::EnvGuard::set("WT_ROOT", root);
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", root.join("state"));
    TEST_OCCUPANTS.with(|c| *c.borrow_mut() = Some(BTreeMap::new()));

    let cfg = bn_core::test_support::config(root);
    let ctx = PoolCtx {
        bare: bare.clone(),
        repo: "bn".to_string(),
        pool_dir: root.join("worktree").join("bn").join("_pool"),
        worktree_dir: root.join("worktree"),
        size: 4,
    };
    let leased = acquire_slot(&ctx, "feat", None, None, SlotRole::Worker, true)
        .expect("two worker slots should be available");
    let comparison = leased.comparison.expect("comparison slot");

    assert_eq!(head_ref(&leased.slot), "feat");
    assert_eq!(head_ref(&comparison), "HEAD");
    assert_eq!(
        wt::git_out(&["-C", &comparison.to_string_lossy(), "rev-parse", "HEAD"]),
        wt::git_out(&[
            "--git-dir",
            &bare.to_string_lossy(),
            "rev-parse",
            &default_base(&bare.to_string_lossy())
        ])
    );

    let leases = read_leases(&ctx.pool_dir);
    assert_eq!(leases.slots.len(), 1, "the pair has one owning lease");
    assert_eq!(leases.reserved_slots().len(), 2);

    let probe = comparison.join("comparison-probe.txt");
    std::fs::write(&probe, "keep me\n").unwrap();
    let duplicate = acquire_slot(&ctx, "feat", None, None, SlotRole::Worker, true)
        .expect("an idempotent lookup should return the existing pair");
    assert_eq!(duplicate.comparison.as_deref(), Some(comparison.as_path()));
    assert_eq!(
        std::fs::read_to_string(&probe).unwrap(),
        "keep me\n",
        "looking up a live pair must not reset its comparison checkout"
    );
    std::fs::remove_file(probe).unwrap();

    assert!(matches!(
        crate::wt_pool::release_slot(&cfg, &leased.slot, false),
        SlotRelease::Returned { .. }
    ));
    assert!(read_leases(&ctx.pool_dir).slots.is_empty());
    assert_eq!(head_ref(&leased.slot), "HEAD");
    assert_eq!(head_ref(&comparison), "HEAD");
}

#[test]
fn paired_dispatch_is_all_or_nothing_when_only_one_worker_slot_is_free() {
    let _g = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let bare = root.join("bare").join("bn.git");
    build_repo_into(&root.join("bn-src"), &bare);

    let _wt_root = bn_core::test_support::EnvGuard::set("WT_ROOT", root);
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", root.join("state"));
    TEST_OCCUPANTS.with(|c| *c.borrow_mut() = Some(BTreeMap::new()));

    let ctx = PoolCtx {
        bare,
        repo: "bn".to_string(),
        pool_dir: root.join("worktree").join("bn").join("_pool"),
        worktree_dir: root.join("worktree"),
        size: 2,
    };
    assert!(acquire_slot(&ctx, "feat", None, None, SlotRole::Worker, true).is_none());
    assert!(
        read_leases(&ctx.pool_dir).slots.is_empty(),
        "a failed pair must not leave half a lease"
    );
}

/// Regression: a dead tenancy on a branch must not wedge that branch's dispatches forever.
///
/// The shape that cost three repos eight hours. A repo-setup dispatch leased a slot, died before
/// its agent came up, and left the lease behind. Every later attempt found the ledger's same-branch
/// entry, was handed the slot back as an *existing* tenancy (`fresh: false`), and `bn dispatch`
/// refused it as an occupied worktree — so the setup agent could never start again for that repo.
/// The leaked-lease sweep would have cleared it, but the acquisition path only runs that when the
/// pool comes up **full**, and a pool with free slots never does: the one dead lease was skipped by
/// the sweep that would fix it and honoured by the lookup that must not.
///
/// Note the pool here is nearly empty. That is the whole point — the bug is invisible under
/// pressure and permanent under plenty.
#[test]
fn a_branch_whose_tenancy_died_can_be_dispatched_again() {
    let _env = bn_core::test_support::env_lock();
    let (tmp, bare) = mk_bare();
    TEST_OCCUPANTS.with(|c| *c.borrow_mut() = Some(BTreeMap::new()));

    let wt_root = tmp.path().join("wt");
    let pdir = wt_root.join("bn").join("_pool");
    std::fs::create_dir_all(&pdir).unwrap();

    // One aged lease for `feat`, nobody in the checkout, holder pid immortal (the real-world
    // shape: the recorded holder is a tmux server, not the agent).
    let slot = slot_path(&pdir, 2);
    assert!(add_detached_slot(&bare, &slot.to_string_lossy()));
    let mut leases = Leases::default();
    leases.slots.insert(
        2,
        Lease {
            branch: "feat".into(),
            holder: "tmux".into(),
            pid: std::process::id(),
            at: now_epoch() - LEAKED_LEASE_GRACE_SECS - 1,
            boot_id: String::new(),
            comparison_slot: None,
        },
    );
    write_leases(&pdir, &leases).unwrap();

    let ctx = PoolCtx {
        bare: PathBuf::from(&bare),
        repo: "bn".into(),
        pool_dir: pdir.clone(),
        worktree_dir: wt_root,
        size: 4,
    };

    let leased = acquire_slot(&ctx, "feat", None, None, SlotRole::Worker, false)
        .expect("a branch whose last tenancy ended must be leasable again");
    // `fresh` is the assertion that matters: it is what dispatch reads to decide between seeding a
    // new run and refusing an occupied worktree. Before the fix this was `false` and every retry
    // died there.
    assert!(
        leased.fresh,
        "a dead tenancy is over — the next lease of that branch is a new one"
    );
    assert!(is_live_slot(&leased.slot));
    assert_eq!(head_ref(&leased.slot), "feat");
    let after = read_leases(&pdir);
    assert_eq!(
        after.slots.values().filter(|l| l.branch == "feat").count(),
        1,
        "one lease for the branch, not two"
    );
}

/// …and the idempotent warm hit it is built on still works: a branch whose agent is *live* keeps
/// getting its own slot back, rather than a second checkout opened on the same branch.
///
/// This is the half that must not regress. The fix distinguishes "already running" from "ran here
/// once"; reclaiming a live tenancy would put two agents in one worktree, which is the failure the
/// same-branch hit exists to prevent.
#[test]
fn a_branch_whose_agent_is_live_still_gets_its_own_slot_back() {
    let _env = bn_core::test_support::env_lock();
    let (tmp, bare) = mk_bare();

    let wt_root = tmp.path().join("wt");
    let pdir = wt_root.join("bn").join("_pool");
    std::fs::create_dir_all(&pdir).unwrap();

    let slot = slot_path(&pdir, 2);
    assert!(add_detached_slot(&bare, &slot.to_string_lossy()));
    assert!(switch_into_slot_from(
        &slot.to_string_lossy(),
        &bare,
        "feat",
        None
    ));
    let mut occupants = BTreeMap::new();
    occupants.insert(
        slot,
        vec![crate::agent_lease::Occupant {
            repo: Some("bn".into()),
            branch: Some("feat".into()),
            session_id: Some("session-live".into()),
            tmux_target: "bn:feat".into(),
            kind: "dispatch".into(),
            // Aged past the grace, so only occupancy — not youth — is keeping it alive.
            since: now_epoch() - LEAKED_LEASE_GRACE_SECS - 1,
        }],
    );
    TEST_OCCUPANTS.with(|c| *c.borrow_mut() = Some(occupants));

    let mut leases = Leases::default();
    leases.slots.insert(
        2,
        Lease {
            branch: "feat".into(),
            holder: "tmux".into(),
            pid: std::process::id(),
            at: now_epoch() - LEAKED_LEASE_GRACE_SECS - 1,
            boot_id: String::new(),
            comparison_slot: None,
        },
    );
    write_leases(&pdir, &leases).unwrap();

    let ctx = PoolCtx {
        bare: PathBuf::from(&bare),
        repo: "bn".into(),
        pool_dir: pdir.clone(),
        worktree_dir: wt_root,
        size: 4,
    };

    let leased =
        acquire_slot(&ctx, "feat", None, None, SlotRole::Worker, false).expect("its own slot back");
    assert_eq!(leased.slot, slot_path(&pdir, 2));
    assert!(
        !leased.fresh,
        "a live agent's branch keeps its tenancy — this is a warm hit, not a new run"
    );
}

/// A lease must not fail over a slot's leftover state. The slot below is idle and unoccupied but
/// carries an uncommitted edit to a file the advanced base also changes — exactly what makes
/// `git switch -c <new> main` refuse — and that is what every BigMono slot looked like after the
/// concurrent-warm incident. Nobody owns that dirt, so get resets the slot and retries rather
/// than reporting "branch checked out elsewhere?" to a caller who can do nothing about it.
#[test]
fn acquire_resets_a_dirty_idle_slot_and_retries_the_switch() {
    let _env = bn_core::test_support::env_lock();
    let (tmp, bare) = mk_bare();
    TEST_OCCUPANTS.with(|c| *c.borrow_mut() = Some(BTreeMap::new()));

    let wt_root = tmp.path().join("wt");
    let pdir = wt_root.join("bn").join("_pool");
    std::fs::create_dir_all(&pdir).unwrap();

    // Slot 2 is a live checkout at main's first commit…
    let slot = slot_path(&pdir, 2);
    let slot_s = slot.to_string_lossy().to_string();
    assert!(add_detached_slot(&bare, &slot_s));

    // …then main moves on and rewrites a.txt…
    let other = tmp.path().join("other");
    let other_s = other.to_string_lossy().to_string();
    assert!(git(&[
        "--git-dir",
        &bare,
        "worktree",
        "add",
        &other_s,
        "main"
    ]));
    std::fs::write(other.join("a.txt"), "upstream\n").unwrap();
    assert!(git(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "-C",
        &other_s,
        "commit",
        "-qam",
        "advance"
    ]));
    assert!(git(&[
        "--git-dir",
        &bare,
        "worktree",
        "remove",
        "--force",
        &other_s
    ]));

    // …while the slot still holds an uncommitted edit to that same file.
    std::fs::write(slot.join("a.txt"), "leftover\n").unwrap();
    assert!(
        !switch_into_slot_from(&slot_s, &bare, "probe", None),
        "precondition: the dirty slot refuses a plain switch onto the advanced base"
    );

    let ctx = PoolCtx {
        bare: PathBuf::from(&bare),
        repo: "bn".into(),
        pool_dir: pdir,
        worktree_dir: wt_root,
        size: 4,
    };
    let leased = acquire_slot(&ctx, "new-work", None, None, SlotRole::Worker, false)
        .expect("get self-heals a dirty idle slot instead of failing the lease");
    assert_eq!(leased.slot, slot);
    assert_eq!(head_ref(&slot), "new-work");
    assert_eq!(
        std::fs::read_to_string(slot.join("a.txt")).unwrap(),
        "upstream\n",
        "the leased checkout is at the advanced base, the leftover edit discarded"
    );
    assert_eq!(wt::dirty_count(&slot_s), 0);
}

/// Two warms on one pool at once is the race that emptied the BigMono slots of tracked files. The
/// second one now yields to the first — reporting success, since the running warm is doing the
/// job — instead of interleaving git calls with it.
#[test]
fn warm_yields_to_a_warm_already_running_on_the_pool() {
    let _g = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    let bare = root.join("bare").join("nvim.git");
    build_repo_into(&root.join("nvim-src"), &bare);

    let _wt_root = bn_core::test_support::EnvGuard::set("WT_ROOT", root);
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", root.join("state"));
    TEST_OCCUPANTS.with(|c| *c.borrow_mut() = Some(BTreeMap::new()));

    let pool_dir = root.join("worktree").join("nvim").join("_pool");
    let _running = slot_lock::try_lock_warm(&pool_dir).expect("the first warm takes the lock");

    let cfg = bn_core::test_support::config(root);
    let ctx = bn_core::resolver::Resolved {
        repo: "nvim".to_string(),
        branch: "main".to_string(),
    };
    assert_eq!(
        super::job_pool_warm(&cfg, Some(&ctx)),
        0,
        "yielding to a running warm is not a failed refresh"
    );
    assert!(
        !is_live_slot(&slot_path(&pool_dir, 2)),
        "the yielding warm must not have touched the pool"
    );
}

/// The merged-slot release tests (`wt-cleanup`), split out at the 1000-line ratchet.
#[path = "pool_release_tests.rs"]
mod release;
