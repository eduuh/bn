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
/// reads as "unknown" (never a false alarm), matching a fail-open guard policy.
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

fn read_ram() -> Ram {
    let m = read_meminfo();
    ram_from(
        m.total_mb.unwrap_or(0),
        m.available_mb.unwrap_or(0),
        m.swap_total_mb.unwrap_or(0),
        m.swap_free_mb.unwrap_or(0),
    )
}

/// Read memory + swap as an `Option`-preserving [`MemInfo`], cross-platform: `/proc/meminfo` on
/// Linux, `sysctl`/`vm_stat` on macOS/BSD. Every unreadable signal stays `None` (never a
/// fabricated `0`), so callers can distinguish "unknown" from "genuinely empty". The single
/// mem/swap reader for the whole crate — `machine::sample` and `worker::capacity` both use it.
#[cfg(target_os = "linux")]
pub fn read_meminfo() -> MemInfo {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .map(|s| parse_meminfo(&s))
        .unwrap_or_default()
}

/// See the Linux variant above.
#[cfg(not(target_os = "linux"))]
pub fn read_meminfo() -> MemInfo {
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
    let total_mb = sysctl_u64("hw.memsize").map(|b| b / 1024 / 1024);
    let available_mb = std::process::Command::new("vm_stat")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| parse_vm_stat_free_mb(&String::from_utf8_lossy(&o.stdout)));
    // vm.swapusage reports total + used; convert used → the free figure callers expect.
    let (swap_total_mb, swap_free_mb) = std::process::Command::new("sysctl")
        .args(["-n", "vm.swapusage"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| parse_vm_swapusage(&String::from_utf8_lossy(&o.stdout)))
        .map(|(total, used)| (Some(total), Some(total.saturating_sub(used))))
        .unwrap_or((None, None));
    MemInfo {
        total_mb,
        available_mb,
        swap_total_mb,
        swap_free_mb,
    }
}

/// The 1-minute load average, cross-platform (`/proc/loadavg` on Linux, `sysctl vm.loadavg` on
/// macOS). `None` when the signal can't be read. Shared by `worker::capacity`.
pub fn read_loadavg1() -> Option<f64> {
    read_loadavg().map(|(l1, ..)| l1)
}
