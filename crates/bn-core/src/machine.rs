//! Cross-platform machine metrics and the saturation policy shared with dispatch backpressure.

use serde::Serialize;

/// Per-core 1-min load at/above which the host is "warming" (amber). One busy core per physical
/// core is the point contention starts to bite.
pub const LOAD_PER_CORE_WARN: f64 = 0.7;
/// Per-core 1-min load at/above which the host is saturated (red) — the runqueue is at least as
/// deep as the core count, so new work only makes latency worse.
pub const LOAD_PER_CORE_CRIT: f64 = 1.0;
/// Fraction of used RAM at/above which the host is warming (amber).
pub const RAM_USED_WARN: f64 = 0.85;
/// Fraction of used RAM at/above which the host is saturated (red).
pub const RAM_USED_CRIT: f64 = 0.95;
/// Fraction of swap in use at/above which the host is warming (amber) — any real swap traffic on a
/// dev box means we've overcommitted RAM.
pub const SWAP_WARN: f64 = 0.10;
/// Fraction of swap in use at/above which the host is saturated (red) — the OOM-killer is close.
pub const SWAP_CRIT: f64 = 0.50;

/// CPU load snapshot. Load figures are `Option` because they're best-effort — an unreadable signal
/// reads as "unknown" (never a false alarm), matching the fail-open guard policy in [`crate::scheduler`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Cpu {
    /// 1-minute load average, or `None` if unreadable.
    pub load1: Option<f64>,
    /// 5-minute load average.
    pub load5: Option<f64>,
    /// 15-minute load average.
    pub load15: Option<f64>,
    /// Logical core count (the load denominator).
    pub cores: usize,
    /// `load1 / cores` — the normalized "used" gauge (≈1.0 means fully committed). `None` when
    /// load is unreadable.
    pub load_per_core: Option<f64>,
}

/// RAM snapshot in MB. `used = total - free`; `swap_used = swap_total - swap_free`. Swap used is
/// the real OOM signal (once swap fills, `cargo`/Node get OOM-killed).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Ram {
    pub total_mb: u64,
    pub free_mb: u64,
    pub used_mb: u64,
    pub swap_total_mb: u64,
    pub swap_used_mb: u64,
    /// `used_mb / total_mb` (0..1), or `None` when total is unknown.
    pub used_frac: Option<f64>,
    /// `swap_used_mb / swap_total_mb` (0..1), or `None` when no swap is configured.
    pub swap_used_frac: Option<f64>,
}

/// A coarse traffic-light for the machine-health widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    Warn,
    Critical,
}

impl Level {
    /// The more severe of two levels (`Ok` < `Warn` < `Critical`).
    fn worse(self, other: Level) -> Level {
        if (other as u8) > (self as u8) {
            other
        } else {
            self
        }
    }
}

/// The derived "near saturation" state (#525) — a level plus human-readable reasons the human (and
/// #503 backpressure) can render. `Ok` with no reasons means comfortably below every threshold.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Saturation {
    pub level: Level,
    pub reasons: Vec<String>,
}

/// The full machine-health snapshot served by `GET /api/machine`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Machine {
    pub cpu: Cpu,
    pub ram: Ram,
    pub saturation: Saturation,
}

impl Machine {
    /// Assemble the snapshot and derive its saturation level from the raw CPU + RAM parts. Pure, so
    /// the threshold policy is unit-testable against synthetic inputs.
    pub fn from_parts(cpu: Cpu, ram: Ram) -> Machine {
        let saturation = assess(&cpu, &ram);
        Machine {
            cpu,
            ram,
            saturation,
        }
    }
}

fn assess_load(cpu: &Cpu) -> Option<(Level, String)> {
    let load = cpu.load_per_core?;
    if load >= LOAD_PER_CORE_CRIT {
        Some((
            Level::Critical,
            format!(
                "load {:.2} ≥ {}×cores ({})",
                cpu.load1.unwrap_or(0.0),
                LOAD_PER_CORE_CRIT,
                cpu.cores
            ),
        ))
    } else if load >= LOAD_PER_CORE_WARN {
        Some((
            Level::Warn,
            format!(
                "load {:.2} ≥ {}×cores ({})",
                cpu.load1.unwrap_or(0.0),
                LOAD_PER_CORE_WARN,
                cpu.cores
            ),
        ))
    } else {
        None
    }
}

fn assess_swap(ram: &Ram) -> Option<(Level, String)> {
    let used = ram.swap_used_frac?;
    if used >= SWAP_CRIT {
        Some((
            Level::Critical,
            format!("swap {:.0}% used (OOM risk)", used * 100.0),
        ))
    } else if used >= SWAP_WARN {
        Some((Level::Warn, format!("swap {:.0}% used", used * 100.0)))
    } else {
        None
    }
}

fn assess_ram(ram: &Ram) -> Option<(Level, String)> {
    let used = ram.used_frac?;
    if used >= RAM_USED_CRIT {
        Some((Level::Critical, format!("RAM {:.0}% used", used * 100.0)))
    } else if used >= RAM_USED_WARN {
        Some((Level::Warn, format!("RAM {:.0}% used", used * 100.0)))
    } else {
        None
    }
}

/// Derive saturation from independent load, swap, and RAM assessments.
pub fn assess(cpu: &Cpu, ram: &Ram) -> Saturation {
    let mut level = Level::Ok;
    let mut reasons = Vec::new();
    for assessment in [assess_load(cpu), assess_swap(ram), assess_ram(ram)]
        .into_iter()
        .flatten()
    {
        level = level.worse(assessment.0);
        reasons.push(assessment.1);
    }
    Saturation { level, reasons }
}

/// Build a [`Cpu`] from optional load figures + core count, computing the per-core gauge.
pub fn cpu_from(load: Option<(f64, f64, f64)>, cores: usize) -> Cpu {
    let (load1, load5, load15) = match load {
        Some((a, b, c)) => (Some(a), Some(b), Some(c)),
        None => (None, None, None),
    };
    let load_per_core = load1.filter(|_| cores > 0).map(|l| l / cores as f64);
    Cpu {
        load1,
        load5,
        load15,
        cores,
        load_per_core,
    }
}

/// Build a [`Ram`] from total/free/swap MB figures, computing `used` + the two fractions. `used`
/// saturates at 0 (never underflows) if a reader ever reports free > total.
pub fn ram_from(total_mb: u64, free_mb: u64, swap_total_mb: u64, swap_free_mb: u64) -> Ram {
    let used_mb = total_mb.saturating_sub(free_mb);
    let swap_used_mb = swap_total_mb.saturating_sub(swap_free_mb);
    let used_frac = (total_mb > 0).then(|| used_mb as f64 / total_mb as f64);
    let swap_used_frac = (swap_total_mb > 0).then(|| swap_used_mb as f64 / swap_total_mb as f64);
    Ram {
        total_mb,
        free_mb,
        used_mb,
        swap_total_mb,
        swap_used_mb,
        used_frac,
        swap_used_frac,
    }
}

// ---------------------------------------------------------------------------
// Pure parsers
// ---------------------------------------------------------------------------

/// Parse the three load averages out of Linux `/proc/loadavg` (`"0.50 0.40 0.30 1/234 5678"`).
pub fn parse_proc_loadavg(s: &str) -> Option<(f64, f64, f64)> {
    let mut it = s.split_whitespace();
    let a = it.next()?.parse().ok()?;
    let b = it.next()?.parse().ok()?;
    let c = it.next()?.parse().ok()?;
    Some((a, b, c))
}

/// Parse the three load averages out of macOS/BSD `sysctl -n vm.loadavg`
/// (`"{ 0.50 0.40 0.30 }"`).
pub fn parse_sysctl_loadavg(s: &str) -> Option<(f64, f64, f64)> {
    let inner = s.trim().trim_start_matches('{').trim_end_matches('}');
    let mut it = inner.split_whitespace();
    let a = it.next()?.parse().ok()?;
    let b = it.next()?.parse().ok()?;
    let c = it.next()?.parse().ok()?;
    Some((a, b, c))
}

/// The memory figures parsed from Linux `/proc/meminfo`, in MB. Each is `Option` since a kernel
/// may omit a field (e.g. no swap).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MemInfo {
    pub total_mb: Option<u64>,
    /// `MemAvailable` — the kernel's own estimate of allocatable RAM (better than raw `MemFree`).
    pub available_mb: Option<u64>,
    pub swap_total_mb: Option<u64>,
    pub swap_free_mb: Option<u64>,
}

/// Parse `MemTotal`/`MemAvailable`/`SwapTotal`/`SwapFree` (all in kB) out of `/proc/meminfo` → MB.
/// Pure so the parse is unit-testable without a Linux host.
pub fn parse_meminfo(s: &str) -> MemInfo {
    let field_mb = |label: &str| -> Option<u64> {
        s.lines()
            .find_map(|l| l.strip_prefix(label))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|kb| kb.parse::<u64>().ok())
            .map(|kb| kb / 1024)
    };
    MemInfo {
        total_mb: field_mb("MemTotal:"),
        available_mb: field_mb("MemAvailable:"),
        swap_total_mb: field_mb("SwapTotal:"),
        swap_free_mb: field_mb("SwapFree:"),
    }
}

/// Parse `(total_mb, used_mb)` out of macOS `sysctl -n vm.swapusage`
/// (`"total = 2048.00M  used = 512.00M  free = 1536.00M"`). Values may carry an `M`/`G` suffix.
pub fn parse_vm_swapusage(s: &str) -> Option<(u64, u64)> {
    let field = |key: &str| -> Option<u64> {
        let rest = s.split(key).nth(1)?;
        // rest starts like " = 2048.00M  used = ..."; take the token after '='.
        let tok = rest.trim_start().trim_start_matches('=').trim_start();
        let tok = tok.split_whitespace().next()?;
        parse_size_mb(tok)
    };
    Some((field("total")?, field("used")?))
}

/// Parse a `sysctl`-style size token (`"2048.00M"`, `"2.00G"`, `"512K"`, or a bare number of bytes)
/// into whole MB.
fn parse_size_mb(tok: &str) -> Option<u64> {
    let (num, mult): (&str, f64) = match tok.chars().last() {
        Some('G') | Some('g') => (&tok[..tok.len() - 1], 1024.0),
        Some('M') | Some('m') => (&tok[..tok.len() - 1], 1.0),
        Some('K') | Some('k') => (&tok[..tok.len() - 1], 1.0 / 1024.0),
        _ => (tok, 1.0 / (1024.0 * 1024.0)), // bare bytes
    };
    num.parse::<f64>().ok().map(|v| (v * mult) as u64)
}

/// Parse free RAM (MB) out of `vm_stat` output (macOS/BSD): sum the free + inactive + speculative +
/// purgeable page classes × the page size from the header line. Pure so the (fiddly) format parse
/// is unit-testable without a Mac. `None` if no page count could be read.
pub fn parse_vm_stat_free_mb(s: &str) -> Option<u64> {
    let page_size = s
        .lines()
        .next()
        .and_then(|l| l.split("page size of").nth(1))
        .and_then(|r| r.split_whitespace().next())
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(4096);
    let pages_of = |label: &str| -> Option<u64> {
        s.lines()
            .find_map(|l| l.trim().strip_prefix(label))
            .map(|r| r.trim().trim_end_matches('.').trim())
            .and_then(|n| n.parse::<u64>().ok())
    };
    let free = [
        "Pages free:",
        "Pages inactive:",
        "Pages speculative:",
        "Pages purgeable:",
    ]
    .iter()
    .filter_map(|l| pages_of(l))
    .sum::<u64>();
    (free > 0).then(|| free * page_size / 1024 / 1024)
}

// ---------------------------------------------------------------------------
// Live sampling (the only IO)
// ---------------------------------------------------------------------------

/// Sample the current machine health (#525). Best-effort and cheap: a couple of small `/proc`
/// reads on Linux, a couple of `sysctl`/`vm_stat` spawns on macOS. Unreadable signals read as
/// unknown (`None`) and never raise the saturation level.
pub fn sample() -> Machine {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let cpu = cpu_from(read_loadavg(), cores);
    let ram = read_ram();
    Machine::from_parts(cpu, ram)
}

#[cfg(target_os = "linux")]
fn read_loadavg() -> Option<(f64, f64, f64)> {
    parse_proc_loadavg(&std::fs::read_to_string("/proc/loadavg").ok()?)
}

#[cfg(not(target_os = "linux"))]
fn read_loadavg() -> Option<(f64, f64, f64)> {
    let out = std::process::Command::new("sysctl")
        .args(["-n", "vm.loadavg"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| parse_sysctl_loadavg(&String::from_utf8_lossy(&out.stdout)))
        .flatten()
}

#[cfg(target_os = "linux")]
fn read_ram() -> Ram {
    let info = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .map(|s| parse_meminfo(&s))
        .unwrap_or_default();
    ram_from(
        info.total_mb.unwrap_or(0),
        info.available_mb.unwrap_or(0),
        info.swap_total_mb.unwrap_or(0),
        info.swap_free_mb.unwrap_or(0),
    )
}

#[cfg(not(target_os = "linux"))]
fn read_ram() -> Ram {
    let sysctl_u64 = |key: &str| -> Option<u64> {
        let out = std::process::Command::new("sysctl")
            .args(["-n", key])
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().parse().ok())
            .flatten()
    };
    let total_mb = sysctl_u64("hw.memsize")
        .map(|b| b / 1024 / 1024)
        .unwrap_or(0);
    let free_mb = std::process::Command::new("vm_stat")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| parse_vm_stat_free_mb(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or(0);
    let (swap_total_mb, swap_used_mb) = std::process::Command::new("sysctl")
        .args(["-n", "vm.swapusage"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| parse_vm_swapusage(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or((0, 0));
    // vm.swapusage reports used directly; convert to the free figure `ram_from` expects.
    ram_from(
        total_mb,
        free_mb,
        swap_total_mb,
        swap_total_mb.saturating_sub(swap_used_mb),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_loadavg() {
        assert_eq!(
            parse_proc_loadavg("0.52 0.44 0.30 1/234 5678"),
            Some((0.52, 0.44, 0.30))
        );
        assert_eq!(parse_proc_loadavg(""), None);
        assert_eq!(parse_proc_loadavg("0.5 x 0.3"), None);
    }

    #[test]
    fn parses_sysctl_loadavg() {
        assert_eq!(
            parse_sysctl_loadavg("{ 1.50 1.20 0.90 }"),
            Some((1.50, 1.20, 0.90))
        );
        assert_eq!(
            parse_sysctl_loadavg("{ 0.0 0.0 0.0 }"),
            Some((0.0, 0.0, 0.0))
        );
    }

    #[test]
    fn parses_meminfo() {
        let s = "MemTotal:       16384000 kB\n\
                 MemFree:         1024000 kB\n\
                 MemAvailable:    4096000 kB\n\
                 SwapTotal:       2048000 kB\n\
                 SwapFree:         512000 kB\n";
        let info = parse_meminfo(s);
        assert_eq!(info.total_mb, Some(16_000));
        assert_eq!(info.available_mb, Some(4_000));
        assert_eq!(info.swap_total_mb, Some(2_000));
        assert_eq!(info.swap_free_mb, Some(500));
    }

    #[test]
    fn parses_meminfo_without_swap() {
        let info = parse_meminfo("MemTotal: 8192000 kB\nMemAvailable: 4096000 kB\n");
        assert_eq!(info.total_mb, Some(8_000));
        assert_eq!(info.swap_total_mb, None);
        assert_eq!(info.swap_free_mb, None);
    }

    #[test]
    fn parses_vm_swapusage() {
        assert_eq!(
            parse_vm_swapusage("total = 2048.00M  used = 512.00M  free = 1536.00M"),
            Some((2048, 512))
        );
        assert_eq!(
            parse_vm_swapusage("vm.swapusage: total = 1.00G  used = 0.00M  free = 1.00G"),
            Some((1024, 0))
        );
    }

    #[test]
    fn parses_vm_stat_free() {
        let s = "Mach Virtual Memory Statistics: (page size of 4096 bytes)\n\
                 Pages free:                          262144.\n\
                 Pages inactive:                      262144.\n\
                 Pages speculative:                        0.\n";
        // (262144 + 262144) pages * 4096 / 1MiB = 2048 MB
        assert_eq!(parse_vm_stat_free_mb(s), Some(2048));
    }

    #[test]
    fn cpu_gauge_normalizes_by_cores() {
        let cpu = cpu_from(Some((8.0, 6.0, 4.0)), 8);
        assert_eq!(cpu.load_per_core, Some(1.0));
        // No load → no gauge.
        assert_eq!(cpu_from(None, 8).load_per_core, None);
        // Zero cores never divides by zero.
        assert_eq!(cpu_from(Some((1.0, 1.0, 1.0)), 0).load_per_core, None);
    }

    #[test]
    fn ram_computes_used_and_fracs() {
        let ram = ram_from(16_000, 4_000, 2_000, 500);
        assert_eq!(ram.used_mb, 12_000);
        assert_eq!(ram.swap_used_mb, 1_500);
        assert_eq!(ram.used_frac, Some(0.75));
        assert_eq!(ram.swap_used_frac, Some(0.75));
        // No swap configured → no fraction (not a divide-by-zero).
        assert_eq!(ram_from(8_000, 8_000, 0, 0).swap_used_frac, None);
    }

    #[test]
    fn assess_ok_when_comfortable() {
        let cpu = cpu_from(Some((1.0, 1.0, 1.0)), 8); // 0.125/core
        let ram = ram_from(16_000, 12_000, 4_000, 4_000); // 25% RAM, 0% swap
        let sat = assess(&cpu, &ram);
        assert_eq!(sat.level, Level::Ok);
        assert!(sat.reasons.is_empty());
    }

    #[test]
    fn assess_warns_on_load_and_ram() {
        let cpu = cpu_from(Some((6.0, 6.0, 6.0)), 8); // 0.75/core → warn
        let ram = ram_from(16_000, 2_000, 4_000, 4_000); // 87.5% RAM → warn, 0% swap
        let sat = assess(&cpu, &ram);
        assert_eq!(sat.level, Level::Warn);
        assert_eq!(sat.reasons.len(), 2);
    }

    #[test]
    fn assess_critical_on_swap_full() {
        // The real OOM signal: swap over half full pushes red even with modest load.
        let cpu = cpu_from(Some((1.0, 1.0, 1.0)), 8);
        let ram = ram_from(16_000, 1_000, 4_000, 1_000); // swap 75% used
        let sat = assess(&cpu, &ram);
        assert_eq!(sat.level, Level::Critical);
        assert!(sat.reasons.iter().any(|r| r.contains("swap")));
    }

    #[test]
    fn assess_critical_on_load_saturation() {
        let cpu = cpu_from(Some((147.0, 120.0, 90.0)), 8); // the load-147 melt this session
        let ram = ram_from(16_000, 12_000, 0, 0);
        let sat = assess(&cpu, &ram);
        assert_eq!(sat.level, Level::Critical);
    }

    #[test]
    fn unreadable_signals_never_raise_level() {
        let cpu = cpu_from(None, 8); // load unknown
        let ram = ram_from(0, 0, 0, 0); // memory unknown
        assert_eq!(assess(&cpu, &ram).level, Level::Ok);
    }

    #[test]
    fn from_parts_wires_saturation() {
        let m = Machine::from_parts(
            cpu_from(Some((147.0, 0.0, 0.0)), 8),
            ram_from(16_000, 12_000, 0, 0),
        );
        assert_eq!(m.saturation.level, Level::Critical);
    }
}
