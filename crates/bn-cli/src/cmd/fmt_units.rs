//! Compact unit formatting shared by the commands that print counts, durations and sizes.
//!
//! These lived in `cmd::staff` and were imported from there by `cmd::cost`, which made a general
//! formatter look like part of the staff command's surface. They depend on nothing else.

/// Compact human token count: `48.2k`, `1.3m`, `4.8b`, or the bare number under 1000.
///
/// The billions tier is not hypothetical: every turn of a cached agent session re-reads the whole
/// prefix, so a month of `bn cost` cache-read totals runs to 10^9 and used to render as `4839.4m`.
pub(crate) fn fmt_tokens(t: u64) -> String {
    if t >= 1_000_000_000 {
        format!("{:.1}b", t as f64 / 1_000_000_000.0)
    } else if t >= 1_000_000 {
        format!("{:.1}m", t as f64 / 1_000_000.0)
    } else if t >= 1_000 {
        format!("{:.1}k", t as f64 / 1_000.0)
    } else {
        t.to_string()
    }
}

/// Compact wall-clock from milliseconds: `3m12s`, `4s`, or `250ms`.
pub(crate) fn fmt_wall_ms(ms: u64) -> String {
    let secs = ms / 1000;
    if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else if ms >= 1000 {
        format!("{secs}s")
    } else {
        format!("{ms}ms")
    }
}

/// Human-readable bytes (`3.2 MB`, `948 KB`, `12 B`) for the retention growth report.
pub(crate) fn fmt_bytes(n: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let n = n as f64;
    if n >= MB {
        format!("{:.1} MB", n / MB)
    } else if n >= KB {
        format!("{:.0} KB", n / KB)
    } else {
        format!("{n:.0} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_switch_unit_at_each_thousand_boundary() {
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(1_000), "1.0k");
        assert_eq!(fmt_tokens(1_000_000), "1.0m");
        assert_eq!(fmt_tokens(1_000_000_000), "1.0b");
    }

    #[test]
    fn wall_clock_drops_to_minutes_only_at_a_full_minute() {
        assert_eq!(fmt_wall_ms(250), "250ms");
        assert_eq!(fmt_wall_ms(4_000), "4s");
        assert_eq!(fmt_wall_ms(59_999), "59s");
        assert_eq!(fmt_wall_ms(192_000), "3m12s");
    }

    #[test]
    fn bytes_use_binary_units() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(4096), "4 KB");
        assert_eq!(fmt_bytes(1024 * 1024 * 3 / 2), "1.5 MB");
    }
}
