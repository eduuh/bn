//! Reading registered systemd `--user` timers and mapping each to the `bn` job it runs, so
//! `bn cron` can show what's actually *scheduled* (not just which scripts exist or last ran).
//! Linux/systemd-only: every entry point degrades to an empty list when `systemctl` is absent
//! or the user bus has no timers, so callers never need to special-case the platform.

use std::process::Command;

/// A systemd `--user` timer that triggers a `bn run <job>` service.
#[derive(Debug, Clone)]
pub struct RegisteredTimer {
    pub job: String,
    pub timer_unit: String,
    /// The `OnCalendar` spec, e.g. `*-*-* 00:00:00`. `None` if the timer is monotonic-only.
    pub schedule: Option<String>,
    /// Next fire, epoch seconds. `None` when the timer isn't scheduled to fire again.
    pub next_epoch: Option<i64>,
}

/// Every registered timer that runs a `bn run <job>` service, or `[]` when systemd is
/// unavailable. Timers we can't classify as a bn job (any other service) are skipped.
pub fn registered() -> Vec<RegisteredTimer> {
    let Some(entries) = list_timers_json() else {
        return Vec::new();
    };
    // One `systemctl show` for every timer and service, not two per timer: with ~20 timers the
    // per-unit calls were 41 processes and most of `bn cron`'s wall time.
    let units: Vec<&str> = entries
        .iter()
        .flat_map(|e| [e.unit.as_str(), e.activates.as_str()])
        .collect();
    let props = show_props(&units, &["ExecStart", "TimersCalendar"]);
    let prop = |unit: &str, key: &str| props.get(unit).and_then(|p| p.get(key)).cloned();
    entries
        .into_iter()
        .filter_map(|e| {
            let job = parse_exec_job(&prop(&e.activates, "ExecStart")?)?;
            Some(RegisteredTimer {
                job,
                schedule: prop(&e.unit, "TimersCalendar").and_then(|c| parse_oncalendar(&c)),
                next_epoch: us_to_secs(e.next),
                timer_unit: e.unit,
            })
        })
        .collect()
}

struct TimerEntry {
    next: u64,
    unit: String,
    activates: String,
}

/// `systemctl --user list-timers -o json` → epoch-µs `next`/`last` plus the timer/service names.
/// Active timers only (no `--all`), so a timer disabled by `unregister` disappears at once.
fn list_timers_json() -> Option<Vec<TimerEntry>> {
    let out = run(&["list-timers", "-o", "json", "--no-pager"])?;
    let arr = serde_json::from_str::<serde_json::Value>(&out)
        .ok()?
        .as_array()?
        .to_vec();
    let mut entries = Vec::new();
    for item in arr {
        let unit = item
            .get("unit")
            .and_then(|x| x.as_str())
            .unwrap_or_default();
        let activates = item
            .get("activates")
            .and_then(|x| x.as_str())
            .unwrap_or_default();
        if unit.is_empty() || activates.is_empty() {
            continue;
        }
        entries.push(TimerEntry {
            next: item.get("next").and_then(|x| x.as_u64()).unwrap_or(0),
            unit: unit.to_string(),
            activates: activates.to_string(),
        });
    }
    Some(entries)
}

/// `systemctl --user show -p Id -p <prop>… <units>…` in one call, as `unit → prop → value`.
/// Empty when systemd is unavailable.
fn show_props(
    units: &[&str],
    props: &[&str],
) -> std::collections::HashMap<String, std::collections::HashMap<String, String>> {
    if units.is_empty() {
        return Default::default();
    }
    let mut args = vec!["show", "-p", "Id"];
    for p in props {
        args.extend(["-p", p]);
    }
    args.extend_from_slice(units);
    run(&args).map(|out| parse_show(&out)).unwrap_or_default()
}

/// Parse multi-unit `systemctl show` output: one `Key=value` block per unit, blank-line
/// separated, each naming its unit in `Id=`. A property repeated within a block (several
/// `ExecStart=` lines) keeps its first value, which is what the per-unit `--value` read used.
fn parse_show(
    out: &str,
) -> std::collections::HashMap<String, std::collections::HashMap<String, String>> {
    let mut all = std::collections::HashMap::new();
    for block in out.split("\n\n") {
        let mut id = None;
        let mut props = std::collections::HashMap::new();
        for line in block.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            if k == "Id" {
                id = Some(v.trim().to_string());
            } else {
                props
                    .entry(k.to_string())
                    .or_insert_with(|| v.trim().to_string());
            }
        }
        if let Some(id) = id {
            all.insert(id, props);
        }
    }
    all
}

fn run(args: &[&str]) -> Option<String> {
    let out = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// Stop and unregister a `--user` timer (`systemctl --user disable --now <timer>`). Disable
/// removes the enablement so it stays off across logins; this is the reversible inverse of
/// `jobs/systemd/install.sh`, which re-enables from the still-present unit files. Returns the
/// systemctl error text on failure (including when systemd is unavailable).
pub fn unregister(timer_unit: &str) -> Result<(), String> {
    let out = Command::new("systemctl")
        .arg("--user")
        .args(["disable", "--now", timer_unit])
        .output()
        .map_err(|e| format!("systemctl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn us_to_secs(us: u64) -> Option<i64> {
    (us != 0).then_some((us / 1_000_000) as i64)
}

/// Pull the job name out of a systemd `ExecStart` value, which contains an argv field like
/// `argv[]=/home/you/.bin/bn run refresh-all`. Only `bn run <job>` services qualify.
fn parse_exec_job(exec: &str) -> Option<String> {
    let argv = field(exec, "argv[]=")?;
    let mut toks = argv.split_whitespace();
    let bin = toks.next()?;
    if bin.rsplit('/').next() != Some("bn") {
        return None;
    }
    if toks.next()? != "run" {
        return None;
    }
    toks.next().map(str::to_string)
}

/// Pull the `OnCalendar` spec out of a `TimersCalendar` value, which looks like
/// `{ OnCalendar=*-*-* 00:00:00 ; next_elapse=... }`.
fn parse_oncalendar(cal: &str) -> Option<String> {
    field(cal, "OnCalendar=").map(str::to_string)
}

/// Read a systemd struct field: the text after `key`, up to the next ` ;` separator, trimmed.
fn field<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    let rest = &s[s.find(key)? + key.len()..];
    let end = rest.find(" ;").unwrap_or(rest.len());
    Some(rest[..end].trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_show_maps_each_block_to_its_unit() {
        let out = "Id=bn-pool-warm.timer\n\
            TimersCalendar={ OnCalendar=*-*-* 00/2:40:00 ; next_elapse=Sun 2026-09-27 08:40:00 PDT }\n\
            \n\
            Id=bn-pool-warm.service\n\
            ExecStart={ path=/home/u/bn ; argv[]=/home/u/bn run pool-warm ; ignore_errors=no }\n\
            ExecStart={ path=/bin/true ; argv[]=/bin/true ; ignore_errors=no }\n\
            \n\
            Id=gone.service\n";
        let props = parse_show(out);
        let exec = &props["bn-pool-warm.service"]["ExecStart"];
        assert_eq!(parse_exec_job(exec).as_deref(), Some("pool-warm"));
        let cal = &props["bn-pool-warm.timer"]["TimersCalendar"];
        assert_eq!(parse_oncalendar(cal).as_deref(), Some("*-*-* 00/2:40:00"));
        assert!(props["gone.service"].is_empty());
    }
}
