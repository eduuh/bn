//! `bn machine list` / `bn machine merge` — see [`crate::cli::machine_cmd`].
use crate::*;
use bn_core::machine_state::{
    machine_dirs, merge_machine, resolve_machine_dir, same_machine, LogMerge, MachineDir,
};

/// `bn machine list` — every host with state under the notes tree, newest activity first.
///
/// Flags the current machine, so a second row belonging to *this* laptop under a different name is
/// visible as the split it is rather than looking like another computer.
pub(crate) fn emit_machine_list(cfg: &Config, as_json: bool) {
    let dirs = machine_dirs(&cfg.personal_root);
    let now = bn_core::util::now_epoch_secs();
    if as_json {
        let rows: Vec<_> = dirs
            .iter()
            .map(|d| {
                serde_json::json!({
                    "machine": d.name,
                    "current": same_machine(&d.name, cfg.machine()),
                    "events": d.events,
                    "last_activity": d.last_activity,
                })
            })
            .collect();
        println!(
            "{}",
            Envelope::success("machine-list", serde_json::json!({ "machines": rows })).to_json()
        );
        return;
    }
    if dirs.is_empty() {
        println!("no machine state under {}", cfg.personal_root.display());
        return;
    }
    for d in &dirs {
        let marker = if same_machine(&d.name, cfg.machine()) {
            "*"
        } else {
            " "
        };
        let age = d
            .last_activity
            .map(|ts| format!("{} ago", crate::cmd::cron::ago(now.saturating_sub(ts))))
            .unwrap_or_else(|| "never".to_string());
        println!("{marker} {:<28} {:>7} events · {age}", d.name, d.events);
    }
    if dirs.len() > 1 {
        println!();
        println!(
            "More than one host has state here. If any of these is *this* machine under an old \
             name, fold it in with `bn machine merge <name>`."
        );
    }
}

/// `bn machine merge <from>` — fold another host's history into this one.
pub(crate) fn emit_machine_merge(cfg: &Config, from: &str, dry_run: bool, as_json: bool) {
    if same_machine(from, cfg.machine()) {
        fail(&format!(
            "bn machine merge: '{from}' is this machine — nothing to merge into itself"
        ));
    }
    let dirs = machine_dirs(&cfg.personal_root);
    let Some(source) = dirs.iter().find(|d| same_machine(&d.name, from)) else {
        fail(&format!(
            "bn machine merge: no state for '{from}' under {} (see `bn machine list`)",
            cfg.personal_root.display()
        ));
    };
    let dest = resolve_machine_dir(&cfg.personal_root, cfg.machine());
    let merges = match merge_machine(&dest, &source.path, !dry_run) {
        Ok(merges) => merges,
        Err(error) => fail(&format!("bn machine merge: {error}")),
    };
    report(cfg, from, &merges, source, dry_run, as_json);
}

fn report(
    cfg: &Config,
    from: &str,
    merges: &[LogMerge],
    source: &MachineDir,
    dry_run: bool,
    as_json: bool,
) {
    let added: usize = merges.iter().map(|m| m.added).sum();
    if as_json {
        let logs: Vec<_> = merges
            .iter()
            .map(|m| {
                serde_json::json!({
                    "log": m.log, "kept": m.kept, "added": m.added, "skipped": m.skipped,
                })
            })
            .collect();
        println!(
            "{}",
            Envelope::success(
                "machine-merge",
                serde_json::json!({
                    "from": from, "into": cfg.machine(), "applied": !dry_run,
                    "added": added, "logs": logs,
                }),
            )
            .to_json()
        );
        return;
    }
    let verb = if dry_run { "would add" } else { "added" };
    for m in merges.iter().filter(|m| m.added > 0 || !dry_run) {
        println!(
            "  {:<24} {verb} {:>6} · had {} · already present {}",
            m.log, m.added, m.kept, m.skipped
        );
    }
    if added == 0 {
        println!("Nothing to merge — '{from}' has no history this machine lacks.");
        return;
    }
    if dry_run {
        println!("\n{verb} {added} line(s) from '{from}'. Re-run without --dry-run to apply.");
        return;
    }
    println!("\nMerged {added} line(s) from '{from}' into '{}'.", cfg.machine());
    println!(
        "'{from}' is left untouched at {} — check the result, then remove it yourself.",
        source.path.display()
    );
}

/// `bn machine join | leave | mesh`: the `mesh` job script with these arguments, attached to this
/// terminal. A shell job because the rest of the mesh plumbing (tunnel-host, tunnel-auto) is one,
/// and they share its helpers. The exit code is the script's.
pub(crate) fn run_mesh(cfg: &Config, args: &[String]) -> i32 {
    let Some((_, script)) = bn_core::resolver::job_script(cfg, None, "mesh") else {
        eprintln!("bn machine: the mesh job (config/jobs/mesh.sh) is not installed; update bn");
        return 2;
    };
    std::process::Command::new("sh")
        .arg(&script)
        .args(args)
        .status()
        .ok()
        .and_then(|s| s.code())
        .unwrap_or(1)
}
