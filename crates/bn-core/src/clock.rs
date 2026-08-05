//! Injectable clock seam. The non-deterministic time inputs features lean on — today's date,
//! wall-clock epoch, formatted stamps — are produced behind a [`Clock`] trait so tests can pin
//! them with a [`FixedClock`]. Entry points construct a [`RealClock`] (the default), which
//! delegates to the env-aware helpers in [`crate::util`], so real behaviour is unchanged.

/// Source of "now" for date- and time-dependent logic (forecasts, daily date, event stamps).
/// The real impl reads the wall clock (honoring `$BN_TODAY` / `$BN_NOW`); [`FixedClock`] returns
/// canned values for deterministic tests.
pub trait Clock {
    /// Today's date as `YYYY-MM-DD`.
    fn today(&self) -> String;
    /// Current wall-clock time as Unix epoch seconds.
    fn now_epoch_secs(&self) -> i64;
    /// Local wall-clock as `YYYY-MM-DD HH:MM` (the `bn log-progress` stamp).
    fn now_stamp(&self) -> String;
    /// A filename-safe local timestamp (`YYYYMMDD-HHMMSS`) for grabbed captures.
    fn now_filestamp(&self) -> String;
}

/// The production clock: delegates to [`crate::util`], which reads the real wall clock (and
/// already honors `$BN_TODAY` / `$BN_NOW` overrides). This is what entry points wire by default,
/// so behaviour is identical to calling the `util` helpers directly.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealClock;

impl Clock for RealClock {
    fn today(&self) -> String {
        crate::util::today()
    }
    fn now_epoch_secs(&self) -> i64 {
        crate::util::now_epoch_secs()
    }
    fn now_stamp(&self) -> String {
        crate::util::now_stamp()
    }
    fn now_filestamp(&self) -> String {
        crate::util::now_filestamp()
    }
}

/// A clock pinned to a fixed epoch second, for deterministic tests. `today` / `now_stamp` /
/// `now_filestamp` are derived from that epoch in local time, matching how the real helpers
/// render `$BN_NOW`.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock {
    epoch: i64,
}

impl FixedClock {
    /// A clock frozen at `epoch` Unix seconds.
    pub fn at(epoch: i64) -> Self {
        Self { epoch }
    }

    fn local(&self) -> Option<chrono::DateTime<chrono::Local>> {
        chrono::DateTime::from_timestamp(self.epoch, 0).map(|dt| dt.with_timezone(&chrono::Local))
    }
}

impl Clock for FixedClock {
    fn today(&self) -> String {
        self.local()
            .map(|dt| dt.format("%Y-%m-%d").to_string())
            .unwrap_or_default()
    }
    fn now_epoch_secs(&self) -> i64 {
        self.epoch
    }
    fn now_stamp(&self) -> String {
        self.local()
            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default()
    }
    fn now_filestamp(&self) -> String {
        self.local()
            .map(|dt| dt.format("%Y%m%d-%H%M%S").to_string())
            .unwrap_or_default()
    }
}
