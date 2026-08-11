use super::*;
use std::process::Command;

fn git(args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Build a one-commit repo on `main` (with a `feat` branch and a committed `.gitignore`) under
/// `srcdir`, then bare-clone it to `bare`. Shared by the tempdir and staged-layout setups.
fn build_repo_into(srcdir: &Path, bare: &Path) {
    std::fs::create_dir_all(srcdir).unwrap();
    let s = srcdir.to_string_lossy().to_string();
    assert!(git(&["init", "-q", &s]));
    assert!(git(&["-C", &s, "config", "user.email", "t@t"]));
    assert!(git(&["-C", &s, "config", "user.name", "t"]));
    std::fs::write(srcdir.join(".gitignore"), "target/\n").unwrap();
    std::fs::write(srcdir.join("a.txt"), "hi\n").unwrap();
    assert!(git(&["-C", &s, "add", "-A"]));
    assert!(git(&["-C", &s, "commit", "-qm", "init"]));
    // Normalize the default branch name across git versions.
    assert!(git(&["-C", &s, "branch", "-M", "main"]));
    assert!(git(&["-C", &s, "branch", "feat"]));
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    assert!(git(&["clone", "-q", "--bare", &s, &bare.to_string_lossy()]));
}

/// A bare clone of a one-commit repo on `main`, with a second branch `feat` and a committed
/// `.gitignore` (`target/`). Returns `(tempdir, bare_path_string)`.
fn mk_bare() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let bare = tmp.path().join("bare.git");
    build_repo_into(&tmp.path().join("src"), &bare);
    (tmp, bare.to_string_lossy().to_string())
}

fn head_ref(slot: &Path) -> String {
    let out = Command::new("git")
        .args([
            "-C",
            &slot.to_string_lossy(),
            "rev-parse",
            "--abbrev-ref",
            "HEAD",
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn default_base_prefers_main() {
    let (_tmp, bare) = mk_bare();
    // A bare clone copies heads (refs/heads/main) but sets up no origin remote, so `main`
    // resolves and `origin/main` does not — default_base must land on `main`.
    assert_eq!(default_base(&bare), "main");
}

#[test]
fn add_detached_slot_creates_a_live_worktree() {
    let (tmp, bare) = mk_bare();
    let slot = tmp.path().join("slot2");
    assert!(add_detached_slot(&bare, &slot.to_string_lossy()));
    assert!(is_live_slot(&slot));
    // Detached at the base — abbrev-ref reads "HEAD".
    assert_eq!(head_ref(&slot), "HEAD");
    assert!(slot.join("a.txt").exists());
}

#[test]
fn switch_reuses_an_existing_branch() {
    let (tmp, bare) = mk_bare();
    let slot = tmp.path().join("slot2");
    assert!(add_detached_slot(&bare, &slot.to_string_lossy()));
    assert!(switch_into_slot(&slot.to_string_lossy(), &bare, "feat"));
    assert_eq!(head_ref(&slot), "feat");
}

#[test]
fn switch_creates_a_new_branch_when_absent() {
    let (tmp, bare) = mk_bare();
    let slot = tmp.path().join("slot2");
    assert!(add_detached_slot(&bare, &slot.to_string_lossy()));
    assert!(switch_into_slot(
        &slot.to_string_lossy(),
        &bare,
        "brand-new"
    ));
    assert_eq!(head_ref(&slot), "brand-new");
}

#[test]
fn switch_fails_when_branch_checked_out_elsewhere() {
    let (tmp, bare) = mk_bare();
    // `feat` is checked out in another worktree; git refuses to check it out a second time.
    let other = tmp.path().join("other");
    assert!(git(&[
        "--git-dir",
        &bare,
        "worktree",
        "add",
        &other.to_string_lossy(),
        "feat"
    ]));
    let slot = tmp.path().join("slot2");
    assert!(add_detached_slot(&bare, &slot.to_string_lossy()));
    assert!(!switch_into_slot(&slot.to_string_lossy(), &bare, "feat"));
}

#[test]
fn is_live_slot_rejects_a_bare_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("not-a-worktree");
    std::fs::create_dir_all(&dir).unwrap();
    // A plain dir (an interrupted `worktree add` leaves files but no `.git`) is not reusable.
    assert!(!is_live_slot(&dir));
    assert!(!is_live_slot(&tmp.path().join("missing")));
}

#[test]
fn force_remove_tears_down_a_slot() {
    let (tmp, bare) = mk_bare();
    let slot = tmp.path().join("slot2");
    assert!(add_detached_slot(&bare, &slot.to_string_lossy()));
    assert!(slot.is_dir());
    force_remove_worktree(&bare, &slot);
    assert!(!slot.exists());
    // The worktree registration is gone too, so the slot can be cleanly recreated.
    let list = Command::new("git")
        .args(["--git-dir", &bare, "worktree", "list", "--porcelain"])
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&list.stdout).contains("slot2"));
}

#[test]
fn force_remove_recovers_a_broken_checkout() {
    let (tmp, bare) = mk_bare();
    let slot = tmp.path().join("slot2");
    assert!(add_detached_slot(&bare, &slot.to_string_lossy()));
    // Simulate an interrupted create: drop the `.git` link so it reads as broken.
    std::fs::remove_file(slot.join(".git")).unwrap();
    assert!(!is_live_slot(&slot));
    force_remove_worktree(&bare, &slot);
    // The recreate path (used by both get and warm) then rebuilds a live slot.
    assert!(add_detached_slot(&bare, &slot.to_string_lossy()));
    assert!(is_live_slot(&slot));
}

#[test]
fn warm_reset_discards_tracked_but_keeps_ignored_deps() {
    let (tmp, bare) = mk_bare();
    let slot = tmp.path().join("slot2");
    assert!(add_detached_slot(&bare, &slot.to_string_lossy()));
    assert!(switch_into_slot(&slot.to_string_lossy(), &bare, "feat"));
    // A tracked edit, an untracked scratch file, and an ignored build artifact.
    std::fs::write(slot.join("a.txt"), "DIRTY\n").unwrap();
    std::fs::write(slot.join("untracked.txt"), "scratch\n").unwrap();
    std::fs::create_dir_all(slot.join("target")).unwrap();
    std::fs::write(slot.join("target/build-cache"), "expensive\n").unwrap();

    warm_reset(&slot.to_string_lossy(), &bare);

    // Tracked change reverted, untracked scratch cleaned…
    assert_eq!(std::fs::read_to_string(slot.join("a.txt")).unwrap(), "hi\n");
    assert!(!slot.join("untracked.txt").exists());
    // …but the ignored deps/build cache — the entire point of the pool — survive.
    assert!(slot.join("target/build-cache").exists());
    // And the slot is detached (ready for the controller / next lease).
    assert_eq!(head_ref(&slot), "HEAD");
}

#[test]
fn wt_cleanup_inside_pool_slot_returns_the_slot() {
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
    assert!(switch_into_slot(
        &slot.to_string_lossy(),
        &bare.to_string_lossy(),
        "feat"
    ));
    let nested = slot.join("nested");
    std::fs::create_dir_all(&nested).unwrap();

    let mut leases = Leases::default();
    leases.slots.insert(
        2,
        Lease {
            branch: "feat".into(),
            holder: "test".into(),
            pid: std::process::id(),
            at: now_epoch(),
            boot_id: String::new(),
        },
    );
    write_leases(&pdir, &leases).unwrap();

    let cfg = bn_core::test_support::config(root);
    assert!(crate::wt::cleanup_one(&cfg, &nested, &root.join("worktree"), false).is_ok());

    assert!(slot.is_dir(), "cleanup should keep the warm slot checkout");
    assert!(read_leases(&pdir).slots.is_empty());
    assert_eq!(head_ref(&slot), "HEAD");
}

/// Regression for the warm-hit-on-a-dead-checkout bug: a lease can outlive its slot dir (removed
/// out-of-band while the holder pid is still alive, so `reclaim_stale` keeps the lease). The
/// idempotent "warm hit" must rebuild the checkout, never hand back the missing path.
#[test]
fn acquire_repairs_a_lease_whose_checkout_vanished() {
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

    let leased = acquire_slot(&ctx, "feat", None, SlotRole::Worker)
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

    let leased = acquire_slot(&ctx, "feat", None, SlotRole::Worker)
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
/// the sweep above from switching a branch out from under somebody.
#[test]
fn acquire_never_reclaims_an_occupied_slot() {
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

    assert!(
        acquire_slot(&ctx, "feat", None, SlotRole::Worker).is_none(),
        "a pool whose every slot holds a live agent is genuinely full"
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
    let (tmp, bare) = mk_bare();
    TEST_OCCUPANTS.with(|c| *c.borrow_mut() = Some(BTreeMap::new()));

    let wt_root = tmp.path().join("wt");
    let pdir = wt_root.join("bn").join("_pool");
    std::fs::create_dir_all(&pdir).unwrap();

    // Pre-build slot 2 as a live checkout on feat, and record its lease.
    let slot = slot_path(&pdir, 2);
    assert!(add_detached_slot(&bare, &slot.to_string_lossy()));
    assert!(switch_into_slot(&slot.to_string_lossy(), &bare, "feat"));
    let mut leases = Leases::default();
    leases.slots.insert(
        2,
        Lease {
            branch: "feat".into(),
            holder: "me".into(),
            pid: std::process::id(),
            at: now_epoch(),
            boot_id: String::new(),
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
    let leased = acquire_slot(&ctx, "feat", None, SlotRole::Worker).expect("warm hit");
    assert!(!leased.fresh);
    assert_eq!(leased.slot, slot);
    assert!(is_live_slot(&leased.slot));
    assert_eq!(head_ref(&leased.slot), "feat");
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

    // Pool `bn` at size 2 via `$WT_POOLS`.
    let pools = root.join("pools");
    std::fs::write(&pools, "bn 2\n").unwrap();

    let _wt_root = bn_core::test_support::EnvGuard::set("WT_ROOT", root);
    let _wt_pools = bn_core::test_support::EnvGuard::set("WT_POOLS", &pools);
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
    assert!(pool_warm(&cfg, Some("bn".to_string()), true, true));

    // The broken stub is now a real, live checkout — the whole point of the fix.
    assert!(is_live_slot(&pool_dir.join("2")));
}

/// Regression for the `pool-warm` cron failing on a pool `bn dispatch` created implicitly.
///
/// `bn dispatch` pools *every* repo with a bn-managed bare clone, registered in `~/.config/wt/pools`
/// or not, and then queues `pool-warm --repo <that repo>` to fill the slot. The cron resolved
/// through [`resolve_pool`], which refuses an unregistered repo — so the warm died exit 1 on a pool
/// sitting right there on disk, and re-queued itself on the next dispatch. Here `nvim` has a bare
/// clone and a pools file that names only `bn`: the cron must warm it and exit 0.
#[test]
fn cron_warms_a_pool_dispatch_created_for_an_unregistered_repo() {
    let _g = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    let bare = root.join("bare").join("nvim.git");
    build_repo_into(&root.join("nvim-src"), &bare);

    // The config knows about `bn` only — `nvim` is pooled by dispatch alone, at the default size.
    let pools = root.join("pools");
    std::fs::write(&pools, "bn 2\n").unwrap();
    assert!(bn_core::pool::pool_size("nvim").is_none());

    let _wt_root = bn_core::test_support::EnvGuard::set("WT_ROOT", root);
    let _wt_pools = bn_core::test_support::EnvGuard::set("WT_POOLS", &pools);
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", root.join("state"));
    TEST_OCCUPANTS.with(|c| *c.borrow_mut() = Some(BTreeMap::new()));

    let cfg = bn_core::test_support::config(root);
    let ctx = bn_core::resolver::Resolved {
        repo: "nvim".to_string(),
        branch: "main".to_string(),
    };
    assert_eq!(super::job_pool_warm(&cfg, Some(&ctx)), 0);

    // Not just a green exit — the slots the next dispatch leases are actually there.
    let pool_dir = root.join("worktree").join("nvim").join("_pool");
    for n in 2..=bn_core::pool::DEFAULT_DISPATCH_POOL_SIZE as u32 {
        assert!(
            is_live_slot(&slot_path(&pool_dir, n)),
            "slot {n} should hold a live checkout"
        );
    }
}

/// The other half of [`cron_warms_a_pool_dispatch_created_for_an_unregistered_repo`]: a queued warm
/// can outlive the clone it named. That is a stale queue entry, not a broken cron, so it reports a
/// skip and exits 0 rather than paging as a cron failure.
#[test]
fn cron_warm_skips_a_repo_with_no_bare_clone() {
    let _g = bn_core::test_support::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("bare")).unwrap();

    let pools = root.join("pools");
    std::fs::write(&pools, "bn 2\n").unwrap();
    let _wt_root = bn_core::test_support::EnvGuard::set("WT_ROOT", root);
    let _wt_pools = bn_core::test_support::EnvGuard::set("WT_POOLS", &pools);
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", root.join("state"));

    let cfg = bn_core::test_support::config(root);
    let ctx = bn_core::resolver::Resolved {
        repo: "long-gone".to_string(),
        branch: "main".to_string(),
    };
    assert_eq!(super::job_pool_warm(&cfg, Some(&ctx)), 0);
}

/// The queue tick's pool reconcile must make a pool's ledger match its slots without a dispatch
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
    let pools = root.join("pools");
    std::fs::write(&pools, "bn 3\n").unwrap();

    let _wt_root = bn_core::test_support::EnvGuard::set("WT_ROOT", root);
    let _wt_pools = bn_core::test_support::EnvGuard::set("WT_POOLS", &pools);
    let _state = bn_core::test_support::EnvGuard::set("BN_STATE_DIR", root.join("state"));

    let pool_dir = root.join("worktree").join("bn").join("_pool");
    std::fs::create_dir_all(&pool_dir).unwrap();
    let bare_s = bare.to_string_lossy().to_string();
    for n in 2..=3 {
        assert!(add_detached_slot(
            &bare_s,
            &slot_path(&pool_dir, n).to_string_lossy()
        ));
    }

    // Slot 2: leased, aged, and nobody in it. Slot 3: no lease at all, but a live agent is working
    // in it — the pair the reconcile has to fix in opposite directions.
    let mut leases = Leases::default();
    leases.slots.insert(
        2,
        Lease {
            branch: "gone".into(),
            holder: "tmux".into(),
            pid: std::process::id(), // immortal holder — the pid check would never free this
            at: now_epoch() - LEAKED_LEASE_GRACE_SECS - 1,
            boot_id: String::new(),
        },
    );
    write_leases(&pool_dir, &leases).unwrap();
    TEST_OCCUPANTS.with(|c| {
        *c.borrow_mut() = Some(BTreeMap::from([(
            slot_path(&pool_dir, 3),
            vec![crate::agent_lease::Occupant {
                repo: Some("bn".into()),
                branch: Some("busy".into()),
                session_id: Some("session-busy".into()),
                tmux_target: "bn:busy".into(),
                kind: "dispatch".into(),
                since: now_epoch() - 60,
            }],
        )]))
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
}

/// The one reclaim path has to be safe to run twice, because it now runs from a queue entry that
/// can be re-filed while an earlier reclaim (or `wt pool return`, or the reaper) is already in
/// flight. An unleased slot is the goal state, so it reports success rather than "refused" — a
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

    let leased = acquire_slot(&ctx, "feat", None, SlotRole::Worker)
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
    let (tmp, bare) = mk_bare();

    let wt_root = tmp.path().join("wt");
    let pdir = wt_root.join("bn").join("_pool");
    std::fs::create_dir_all(&pdir).unwrap();

    let slot = slot_path(&pdir, 2);
    assert!(add_detached_slot(&bare, &slot.to_string_lossy()));
    assert!(switch_into_slot(&slot.to_string_lossy(), &bare, "feat"));
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

    let leased = acquire_slot(&ctx, "feat", None, SlotRole::Worker).expect("its own slot back");
    assert_eq!(leased.slot, slot_path(&pdir, 2));
    assert!(
        !leased.fresh,
        "a live agent's branch keeps its tenancy — this is a warm hit, not a new run"
    );
}
