use std::fmt::Display;
use std::time::Duration;

pub(crate) const MAX_ATTEMPTS: usize = 4;
pub(crate) const INITIAL_BACKOFF: Duration = Duration::from_millis(250);
pub(crate) const MAX_BACKOFF: Duration = Duration::from_secs(4);

pub(crate) fn backoff_for_retry(retry: usize) -> Duration {
    let exponent = retry.saturating_sub(1).min(31) as u32;
    INITIAL_BACKOFF
        .checked_mul(4_u32.saturating_pow(exponent))
        .unwrap_or(MAX_BACKOFF)
        .min(MAX_BACKOFF)
}

pub(crate) fn total_backoff() -> Duration {
    (1..MAX_ATTEMPTS).map(backoff_for_retry).sum()
}

pub(crate) fn io_error_is_transient(error: &std::io::Error) -> bool {
    if matches!(
        error.kind(),
        std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::TimedOut
    ) {
        return true;
    }

    #[cfg(unix)]
    return error.raw_os_error().is_some_and(|code| {
        matches!(
            code,
            libc::EAGAIN | libc::ENOMEM | libc::ETXTBSY | libc::ENFILE | libc::EMFILE
        )
    });

    #[cfg(not(unix))]
    false
}

pub(crate) fn run<T, E, Operation, IsTransient, Sleep, Log>(
    label: &str,
    mut operation: Operation,
    is_transient: IsTransient,
    mut sleep: Sleep,
    mut log: Log,
) -> Result<T, E>
where
    E: Display,
    Operation: FnMut() -> Result<T, E>,
    IsTransient: Fn(&E) -> bool,
    Sleep: FnMut(Duration),
    Log: FnMut(String),
{
    for attempt in 1..=MAX_ATTEMPTS {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) if attempt < MAX_ATTEMPTS && is_transient(&error) => {
                let delay = backoff_for_retry(attempt);
                log(format!(
                    "{label}: transient failure on attempt {attempt}/{MAX_ATTEMPTS}: {error}; \
                     retrying in {}ms (bounded to {} attempts / {}ms total backoff)",
                    delay.as_millis(),
                    MAX_ATTEMPTS,
                    total_backoff().as_millis()
                ));
                sleep(delay);
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("retry loop always returns on its final attempt")
}

pub(crate) fn io<T>(
    label: &str,
    operation: impl FnMut() -> std::io::Result<T>,
) -> std::io::Result<T> {
    run(
        label,
        operation,
        io_error_is_transient,
        std::thread::sleep,
        |message| eprintln!("{message}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn transient_failure_retries_and_succeeds() {
        let attempts = Cell::new(0);
        let sleeps = RefCell::new(Vec::new());
        let logs = RefCell::new(Vec::new());

        let result = run(
            "launch copilot",
            || {
                attempts.set(attempts.get() + 1);
                if attempts.get() < 3 {
                    Err("resource temporarily unavailable")
                } else {
                    Ok("started")
                }
            },
            |_| true,
            |delay| sleeps.borrow_mut().push(delay),
            |message| logs.borrow_mut().push(message),
        );

        assert_eq!(result, Ok("started"));
        assert_eq!(attempts.get(), 3);
        assert_eq!(
            *sleeps.borrow(),
            [Duration::from_millis(250), Duration::from_secs(1)]
        );
        assert_eq!(logs.borrow().len(), 2);
        assert!(logs.borrow()[0].contains("attempt 1/4"));
        assert!(logs.borrow()[0].contains("retrying in 250ms"));
        assert!(logs.borrow()[0].contains("5250ms total backoff"));
    }

    #[test]
    fn permanent_failure_fails_without_retry() {
        let attempts = Cell::new(0);
        let result = run(
            "launch unknown harness",
            || {
                attempts.set(attempts.get() + 1);
                Err::<(), _>("unknown harness")
            },
            |_| false,
            |_| panic!("permanent failures must not sleep"),
            |_| panic!("permanent failures must not log a retry"),
        );

        assert_eq!(result, Err("unknown harness"));
        assert_eq!(attempts.get(), 1);
    }

    #[test]
    fn backoff_is_capped_and_retry_window_is_bounded() {
        assert_eq!(backoff_for_retry(1), Duration::from_millis(250));
        assert_eq!(backoff_for_retry(2), Duration::from_secs(1));
        assert_eq!(backoff_for_retry(3), Duration::from_secs(4));
        assert_eq!(backoff_for_retry(20), MAX_BACKOFF);

        assert_eq!(total_backoff(), Duration::from_millis(5_250));
    }
}
