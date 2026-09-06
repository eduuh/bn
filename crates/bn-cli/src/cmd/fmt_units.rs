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

}
