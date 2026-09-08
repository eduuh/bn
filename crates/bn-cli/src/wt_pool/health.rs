//! Pool health: the warm stamps that say which slots have a completed build behind them, and the
//! `pool metrics` health report that names every slot failing the "instant lease" promise.
//!
//! Split out of `wt_pool.rs` under the 1000-line ratchet.
use super::*;

// --- warm stamps (<pool_dir>/warm/<n>.json) --------------------------------
//
// "Warm" used to mean only "a live checkout exists", so a lease could land in a slot whose deps
// were never installed — warm had skipped it as dirty, or its build had failed — while a built
// slot sat idle one number up. A stamp is written by the warm path on a *successful* build and
// cleared whenever the checkout is recreated (a fresh `worktree add` has no deps), and the pick
// prefers stamped slots. It lives beside `leases.json`, never inside the checkout: an untracked
// file in the worktree would itself make the slot dirty.

fn warm_stamp_path(pool_dir: &Path, n: u32) -> PathBuf {
    pool_dir.join("warm").join(format!("{n}.json"))
}

/// Slots with a warm stamp — the ones whose build actually completed.
pub(crate) fn read_warm_stamps(pool_dir: &Path) -> BTreeSet<u32> {
    let mut out = BTreeSet::new();
    if let Ok(rd) = std::fs::read_dir(pool_dir.join("warm")) {
        for e in rd.flatten() {
            let name = e.file_name();
            let Some(stem) = name.to_str().and_then(|s| s.strip_suffix(".json")) else {
                continue;
            };
            if let Ok(n) = stem.parse::<u32>() {
                out.insert(n);
            }
        }
    }
    out
}

/// Record that slot `n`'s build succeeded at `base` (its HEAD sha, for `pool status`).
pub(crate) fn write_warm_stamp(pool_dir: &Path, n: u32, base: &str) {
    let path = warm_stamp_path(pool_dir, n);
    let _ = std::fs::create_dir_all(pool_dir.join("warm"));
    let data = serde_json::json!({ "base": base, "at": now_epoch() });
    let _ = std::fs::write(path, data.to_string());
}

pub(crate) fn clear_warm_stamp(pool_dir: &Path, n: u32) {
    let _ = std::fs::remove_file(warm_stamp_path(pool_dir, n));
}

// --- dirty marks (<pool_dir>/warm/dirty.json) -------------------------------
//
// The slots the last warm skipped as dirty. A lease must not land in one — it would carry
// somebody's uncommitted work onto a new branch — but asking git costs 12–22 s on a monorepo
// checkout, so the pick reads what warm already found out. A mark is cleared by the reset that
// makes the slot clean again (`warm_reset`), and rewritten on every warm.

fn dirty_marks_path(pool_dir: &Path) -> PathBuf {
    pool_dir.join("warm").join("dirty.json")
}

pub(crate) fn read_dirty_marks(pool_dir: &Path) -> BTreeSet<u32> {
    std::fs::read_to_string(dirty_marks_path(pool_dir))
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<u32>>(&s).ok())
        .unwrap_or_default()
        .into_iter()
        .collect()
}

pub(crate) fn write_dirty_marks(pool_dir: &Path, slots: &[u32]) {
    let _ = std::fs::create_dir_all(pool_dir.join("warm"));
    let _ = std::fs::write(
        dirty_marks_path(pool_dir),
        serde_json::to_string(slots).unwrap_or_else(|_| "[]".to_string()),
    );
}

pub(crate) fn clear_dirty_mark(pool_dir: &Path, n: u32) {
    let mut marks = read_dirty_marks(pool_dir);
    if marks.remove(&n) {
        write_dirty_marks(pool_dir, &marks.into_iter().collect::<Vec<_>>());
    }
}

/// The recovery + health tail of `pool metrics`: what this check just healed (prior-boot slots
/// reclaimed now), the lifetime restart-recovery count, and any leased slot still needing a human
/// (a prior-boot lease left dirty, or a same-boot lease whose holder pid has died). This is the
/// "continuously maintain the health of the leasing worktree" surface — reading it also heals.
pub(super) fn print_pool_health(
    ctx: &PoolCtx,
    leases: &Leases,
    recovered: &ReclaimReport,
    lifetime_recoveries: u64,
) {
    use bn_core::pool::LeaseLiveness;
    if !recovered.prior_boot.is_empty() {
        println!(
            "  recovered   : {} slot(s) reclaimed from a prior boot now — {}",
            recovered.prior_boot.len(),
            slot_list(&recovered.prior_boot)
        );
    }
    if lifetime_recoveries > 0 {
        println!("  boot recov. : {lifetime_recoveries} (lifetime restart recoveries)");
    }

    let current_boot = current_boot_id();
    let mut attention: Vec<String> = Vec::new();
    for (n, l) in &leases.slots {
        let liveness = bn_core::pool::lease_liveness(
            boot_opt(&l.boot_id),
            current_boot.as_deref(),
            pid_alive(l.pid),
        );
        // No git here: whether the slot holds uncommitted work is `return`'s question (it
        // refuses without --force), and a `git status` per dead lease was 6 s each on a monorepo.
        match liveness {
            LeaseLiveness::Live => {}
            LeaseLiveness::StalePriorBoot => attention.push(format!(
                "[{n}] {} — from a prior boot; `wt pool return {n}` (add --force to discard \
                 uncommitted work)",
                l.branch,
            )),
            LeaseLiveness::DeadThisBoot => attention.push(format!(
                "[{n}] {} — holder pid {} gone this boot; `wt pool return {n}` (add --force to \
                 discard uncommitted work)",
                l.branch, l.pid,
            )),
        }
    }
    // The two ways an idle slot fails the "instant lease" promise, both invisible from the
    // ledger: it exists but no build ever completed in it, or warm keeps skipping it because it
    // is dirty. Neither costs a git call here — stamps are files, and the dirty list is what the
    // last warm already found out.
    let built = read_warm_stamps(&ctx.pool_dir);
    let reserved = leases.reserved_slots();
    for n in (bn_core::agent::MAIN_POOL_SLOT + 1)..=ctx.size as u32 {
        if reserved.contains(&n)
            || built.contains(&n)
            || !is_live_slot(&slot_path(&ctx.pool_dir, n))
        {
            continue;
        }
        attention.push(format!(
            "[{n}] idle but never built — a lease here still pays the full install; `wt pool warm`"
        ));
    }
    for n in read_dirty_marks(&ctx.pool_dir) {
        if reserved.contains(&n) {
            continue;
        }
        attention.push(format!(
            "[{n}] dirty and idle — every warm skips it, so it never advances or rebuilds; \
             commit the work or `wt pool return {n} --force`"
        ));
    }
    if attention.is_empty() {
        println!("  health      : ok");
    } else {
        println!("  health      : {} slot(s) need attention", attention.len());
        for line in attention {
            println!("    - {line}");
        }
    }
}
