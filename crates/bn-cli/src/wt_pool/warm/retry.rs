//! Retrying the slot builds that declined to run.
//!
//! A monorepo's `build.sh` refuses a second concurrent `lage build` on the machine, so a warm's parallel
//! pass leaves one slot built and the rest declined. Retrying each declined slot once was not
//! enough: on 2026-09-27 a 12-slot warm still ended 2 warm, 10 not built. So the declined slots are
//! retried one at a time, and a slot that declines again (a build from outside the warm still holds
//! the toolchain) waits and tries again, until it builds, fails, or the budget runs out. A slot
//! build restored from the shared cache takes minutes, so a budget of hours covers a full pool.

use std::time::{Duration, Instant};

use super::BuildOutcome;

/// How long a warm keeps retrying declined builds, unless `BN_POOL_WARM_RETRY_SECS` says otherwise.
const DEFAULT_BUDGET: Duration = Duration::from_secs(4 * 60 * 60);

/// How long to wait before retrying a slot whose serial retry was declined too.
pub(super) const PAUSE: Duration = Duration::from_secs(2 * 60);

/// The retry budget: `BN_POOL_WARM_RETRY_SECS` when it parses as seconds, else four hours. `0`
/// turns the serial retry off.
pub(super) fn budget() -> Duration {
    std::env::var("BN_POOL_WARM_RETRY_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_BUDGET)
}

/// The clock and the sleep, injectable so the policy can be tested without waiting.
pub(super) trait Waiter {
    fn elapsed(&self) -> Duration;
    fn sleep(&mut self, d: Duration);
}

/// The real clock, started when the serial retries start.
pub(super) struct RealWaiter(pub Instant);

impl Waiter for RealWaiter {
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }
    fn sleep(&mut self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// Retry every [`BuildOutcome::Skipped`] slot in `results`, one at a time, in slot order.
///
/// Each gets an immediate retry; one that declines again waits `pause` and retries, as long as the
/// wait would still end inside `budget`. Once the budget is spent the remaining slots keep their
/// `Skipped` outcome, which the caller reports as not built. Built and failed slots are never
/// rebuilt.
pub(super) fn retry_declined(
    results: &mut [(u32, BuildOutcome)],
    budget: Duration,
    pause: Duration,
    waiter: &mut impl Waiter,
    mut build: impl FnMut(u32) -> BuildOutcome,
) {
    results.sort_by_key(|(n, _)| *n);
    for (n, outcome) in results.iter_mut() {
        if *outcome != BuildOutcome::Skipped {
            continue;
        }
        loop {
            if waiter.elapsed() >= budget {
                return;
            }
            *outcome = build(*n);
            if *outcome != BuildOutcome::Skipped {
                break;
            }
            if waiter.elapsed() + pause >= budget {
                return;
            }
            waiter.sleep(pause);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use BuildOutcome::{Built, Failed, Skipped};

    /// A clock that only moves when slept on, or when a build "takes" time.
    struct FakeWaiter {
        now: Duration,
        slept: Vec<Duration>,
    }

    impl Waiter for FakeWaiter {
        fn elapsed(&self) -> Duration {
            self.now
        }
        fn sleep(&mut self, d: Duration) {
            self.slept.push(d);
            self.now += d;
        }
    }

    fn waiter() -> FakeWaiter {
        FakeWaiter {
            now: Duration::ZERO,
            slept: Vec::new(),
        }
    }

    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn declined_slots_are_retried_one_at_a_time_in_slot_order() {
        let mut results = vec![(4, Skipped), (1, Built), (2, Skipped), (3, Failed)];
        let mut calls = Vec::new();
        retry_declined(&mut results, 240 * MIN, 2 * MIN, &mut waiter(), |n| {
            calls.push(n);
            Built
        });
        assert_eq!(calls, vec![2, 4]);
        assert_eq!(
            results,
            vec![(1, Built), (2, Built), (3, Failed), (4, Built)]
        );
    }

    #[test]
    fn a_slot_that_declines_again_waits_and_retries() {
        let mut results = vec![(1, Built), (5, Skipped)];
        let mut tries = 0;
        let mut w = waiter();
        retry_declined(&mut results, 240 * MIN, 2 * MIN, &mut w, |_| {
            tries += 1;
            if tries < 3 {
                Skipped
            } else {
                Built
            }
        });
        assert_eq!(tries, 3);
        assert_eq!(w.slept, vec![2 * MIN, 2 * MIN]);
        assert_eq!(results, vec![(1, Built), (5, Built)]);
    }

    #[test]
    fn a_failed_retry_is_not_retried_again() {
        let mut results = vec![(2, Skipped)];
        let mut tries = 0;
        retry_declined(&mut results, 240 * MIN, 2 * MIN, &mut waiter(), |_| {
            tries += 1;
            Failed
        });
        assert_eq!(tries, 1);
        assert_eq!(results, vec![(2, Failed)]);
    }

    #[test]
    fn the_budget_stops_the_retries_and_leaves_the_rest_declined() {
        let mut results = vec![(2, Skipped), (3, Skipped)];
        let mut calls = Vec::new();
        let mut w = waiter();
        // Always declined: slot 2 retries at 0, 2, 4, 6, 8 min; waiting to 10 would spend the budget.
        retry_declined(&mut results, 10 * MIN, 2 * MIN, &mut w, |n| {
            calls.push(n);
            Skipped
        });
        assert_eq!(calls, vec![2, 2, 2, 2, 2]);
        assert!(w.now < 10 * MIN);
        assert_eq!(results, vec![(2, Skipped), (3, Skipped)]);
    }

    #[test]
    fn a_zero_budget_retries_nothing() {
        let mut results = vec![(2, Skipped)];
        retry_declined(&mut results, Duration::ZERO, 2 * MIN, &mut waiter(), |_| {
            panic!("no retry with a zero budget")
        });
        assert_eq!(results, vec![(2, Skipped)]);
    }
}
