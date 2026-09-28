//! `bn agent checkin` and the exact-time timer it (and any one-shot watchdog) rides on.
//!
//! A check-in is a one-shot `reminder` watchdog: the row in the coordination DB is the durable
//! record, so it survives a reboot and shows up in `bn watchdog list`. The timer armed here only
//! makes it fire on the minute; if the timer is lost, the next `coord-tick` still fires it.
use crate::*;

/// Run `bn <args…>` once, `mins` minutes from now: a systemd `--user` transient timer where
/// systemd exists, else a detached `sleep` (macOS). Best-effort — returns whether it was armed.
/// `args` travel as argv, never through a shell string, so nothing in them can inject.
pub(crate) fn schedule_bn_after(mins: i64, args: &[&str]) -> bool {
    if cfg!(windows) || mins <= 0 {
        return false;
    }
    let bn = std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "bn".into());
    if !cfg!(target_os = "macos") && crate::systemd::have_systemctl() {
        let on_active = format!("--on-active={mins}min");
        let unit = format!("--unit=bn-timer-{}", bn_core::util::now_epoch_secs());
        return std::process::Command::new("systemd-run")
            .args(["--user", "--quiet", &on_active, &unit, &bn])
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
    }
    let secs = (mins * 60).to_string();
    std::process::Command::new("sh")
        .args(["-c", "sleep \"$1\"; shift; exec \"$@\"", "bn-timer", &secs, &bn])
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

/// `bn agent checkin <mins> "<note>"` — in `<mins>` minutes, deliver `<note>` to this worktree's
/// agent (or `--target repo:branch`). Registers a one-shot reminder watchdog, so the check-in is
/// listed by `bn watchdog list`, can be stopped with `bn watchdog stop`, and survives a reboot.
pub(crate) fn emit_agent_checkin(
    cfg: &Config,
    mins: i64,
    note: &str,
    target: Option<String>,
    as_json: bool,
) {
    if mins <= 0 {
        fail("bn agent checkin: <mins> must be a positive number of minutes");
    }
    let here = target_ctx(cfg, None, None).map(|ctx| format!("{}:{}", ctx.repo, ctx.branch));
    let Some(target) = target.or_else(|| here.clone()) else {
        fail("bn agent checkin: not in a git repo — pass --target repo:branch")
    };
    let created_by = here.unwrap_or_else(|| target.clone());
    match crate::cmd::watchdog::checkin(&target, mins as u64, note, &created_by) {
        Ok(wd) => {
            if as_json {
                let data = serde_json::json!({
                    "scheduled": true,
                    "watchdog_id": wd.id,
                    "target": wd.target,
                    "mins": mins,
                    "note": note,
                });
                println!("{}", Envelope::success("agent_checkin", data).to_json());
            } else {
                println!("Check-in {} in {mins}m → {}: {note}", wd.id, wd.target);
            }
        }
        Err(e) => {
            let msg = format!("bn agent checkin: could not schedule the check-in ({e})");
            fail_json(as_json, "agent_checkin", "schedule_failed", &msg)
        }
    }
}
