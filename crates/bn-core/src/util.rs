//! Small shared helpers.

/// Today's date as `YYYY-MM-DD`. Uses `$BN_TODAY` when set (deterministic tests), else the
/// local-timezone date via `chrono` (cross-platform; no `date` binary, which native Windows
/// lacks).
pub fn today() -> String {
    if let Ok(d) = std::env::var("BN_TODAY") {
        if !d.is_empty() {
            return d;
        }
    }
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// Current UTC time as an RFC-3339 `YYYY-MM-DDThh:mm:ssZ` string — the `date -u +…Z` equivalent,
/// used to stamp `generated_at` on the review-queue cache (the `pr-queue` job).
pub fn now_rfc3339_utc() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// A Unix-epoch-seconds instant as the same RFC-3339 UTC string [`now_rfc3339_utc`] produces, or
/// `None` for an out-of-range value. Lets an epoch timestamp bn holds (a pool lease's `at`) be
/// compared against the ISO instants other tools record (a Copilot session's `updated_at`), which
/// sort lexically in this format.
pub fn epoch_to_rfc3339_utc(epoch_secs: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(epoch_secs, 0)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

/// The local-timezone date (`YYYY-MM-DD`) of a Unix-epoch-seconds instant. Mirrors [`today`]'s
/// format and zone, for "did this happen today?" comparisons against event timestamps.
pub fn local_date_of(epoch_secs: i64) -> String {
    chrono::DateTime::from_timestamp(epoch_secs, 0)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_default()
}

/// A compact local `MM-DD HH:MM` rendering of a Unix-epoch-seconds instant — for terse event
/// listings (e.g. `bn pipeline status` transition history).
pub fn fmt_local_short(epoch_secs: i64) -> String {
    chrono::DateTime::from_timestamp(epoch_secs, 0)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

/// Current wall-clock time as Unix epoch seconds. `$BN_NOW` overrides for deterministic
/// tests. Used to stamp appended events.
pub fn now_epoch_secs() -> i64 {
    if let Ok(s) = std::env::var("BN_NOW") {
        if let Ok(n) = s.trim().parse::<i64>() {
            return n;
        }
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

/// Local wall-clock as `YYYY-MM-DD HH:MM` — the stamp `bn log-progress` prefixes to each
/// entry. `$BN_NOW` (epoch seconds) overrides for deterministic tests, matching the rest of
/// the time helpers. Cross-platform via `chrono` (no `date` binary).
pub fn now_stamp() -> String {
    if let Ok(s) = std::env::var("BN_NOW") {
        if let Ok(n) = s.trim().parse::<i64>() {
            if let Some(dt) = chrono::DateTime::from_timestamp(n, 0) {
                return dt
                    .with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string();
            }
        }
    }
    chrono::Local::now().format("%Y-%m-%d %H:%M").to_string()
}

/// A filename-safe local timestamp (`YYYYMMDD-HHMMSS`) for grabbed captures. Honors `BN_NOW`
/// (epoch seconds) so tests are deterministic.
pub fn now_filestamp() -> String {
    if let Ok(s) = std::env::var("BN_NOW") {
        if let Ok(n) = s.trim().parse::<i64>() {
            if let Some(dt) = chrono::DateTime::from_timestamp(n, 0) {
                return dt
                    .with_timezone(&chrono::Local)
                    .format("%Y%m%d-%H%M%S")
                    .to_string();
            }
        }
    }
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

/// A local `YYYY-MM-DD` date for an arbitrary epoch-seconds timestamp. For rendering historical
/// events (e.g. agent sessions) where `today()` — which only knows *now* — won't do.
pub fn date_of(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_default()
}

/// A local `HH:MM` wall-clock time for an arbitrary epoch-seconds timestamp — for rendering when
/// a focus block ends ("ends 15:42"). Empty string for an out-of-range epoch.
pub fn clock_hm(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_default()
}

/// The ISO-8601 week label (`YYYY-Www`, e.g. `2026-W26`) for a `YYYY-MM-DD` date — the
/// filename stem the weekly-insights snapshot accumulates under so history sorts and never
/// collides. Uses the ISO week-year (which can differ from the calendar year in late
/// December / early January). `None` if `date` doesn't parse.
pub fn iso_week(date: &str) -> Option<String> {
    use chrono::Datelike;
    let d = chrono::NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d").ok()?;
    let w = d.iso_week();
    Some(format!("{}-W{:02}", w.year(), w.week()))
}

/// Parse a timestamp string to epoch seconds. Accepts an RFC-3339 instant
/// (`2026-06-20T09:00:00Z`, the shape `gh`'s `createdAt`/`updatedAt` use) or a bare
/// `YYYY-MM-DD` (taken as local midnight). `None` when it parses as neither — so callers can
/// fall back gracefully on an old cache that carries no timestamp.
pub fn parse_ts(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp());
    }
    // Bare date → local midnight.
    use chrono::TimeZone;
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .and_then(|naive| chrono::Local.from_local_datetime(&naive).single())
        .map(|dt| dt.timestamp())
}

/// Identify the agent harness currently driving bn, harness-agnostically.
/// `$BN_HARNESS` is the explicit override (covers Copilot and any future harness);
/// otherwise sniff known environment signatures. `None` outside a known harness
/// (e.g. a plain shell), so nothing is recorded then.
pub fn detect_harness() -> Option<String> {
    if let Ok(h) = std::env::var("BN_HARNESS") {
        let h = h.trim();
        if !h.is_empty() {
            return Some(h.to_string());
        }
    }
    // Claude Code exports CLAUDECODE=1 and CLAUDE_CODE_ENTRYPOINT.
    if std::env::var_os("CLAUDECODE").is_some()
        || std::env::var_os("CLAUDE_CODE_ENTRYPOINT").is_some()
    {
        return Some("claude".to_string());
    }
    // GitHub Copilot CLI exports COPILOT_RUN_APP=1 and COPILOT_CLI_BINARY_VERSION
    // into the shell its tools run in.
    if std::env::var_os("COPILOT_RUN_APP").is_some()
        || std::env::var_os("COPILOT_CLI_BINARY_VERSION").is_some()
    {
        return Some("copilot".to_string());
    }
    None
}

/// Identify the model the active harness is running, if exposed. Resolves in order:
/// `$BN_MODEL` (explicit override a launcher can export); the per-session model file
/// (keyed by session id — Claude Code); the per-cwd model file (keyed by the worktree —
/// for harnesses like Copilot that don't put a session id in the tool env); then
/// best-effort env signatures (`ANTHROPIC_MODEL`, …). A harness hook (re)writes the
/// file each turn from the live model, so mid-session switches are caught. `None` when
/// nothing identifies the model.
pub fn detect_model() -> Option<String> {
    if let Some(m) = env_nonempty("BN_MODEL") {
        return Some(m);
    }
    for path in [session_model_file(), cwd_model_file()]
        .into_iter()
        .flatten()
    {
        if let Ok(s) = std::fs::read_to_string(&path) {
            let s = s.trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    for key in [
        "ANTHROPIC_MODEL",
        "CLAUDE_MODEL",
        "OPENAI_MODEL",
        "COPILOT_MODEL",
    ] {
        if let Some(m) = env_nonempty(key) {
            return Some(m);
        }
    }
    None
}

/// Path of the per-session "current model" file under the bn state dir, keyed by
/// session id so concurrent sessions/worktrees don't clobber each other. A harness
/// hook (re)writes it each turn; `detect_model` reads it. `BN_SESSION_ID` is the
/// generic key (any harness); Claude Code's `CLAUDE_CODE_SESSION_ID` is accepted too.
/// `None` when no session id is in the environment.
pub fn session_model_file() -> Option<std::path::PathBuf> {
    let sid = env_nonempty("BN_SESSION_ID").or_else(|| env_nonempty("CLAUDE_CODE_SESSION_ID"))?;
    Some(crate::resolver::state_dir().join(format!("model-{sid}")))
}

/// Path of the per-cwd "current model" file, keyed by the current directory. Used by
/// harnesses with no session id in the tool environment (Copilot): the hook keys by
/// its payload `cwd`, bn by its own cwd — equal under one-agent-per-worktree. `None`
/// if the cwd can't be read. Keep the key derivation identical to the hook side
/// (`model-cwd-<path with '/'→'_'>`).
pub fn cwd_model_file() -> Option<std::path::PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let key = cwd.to_string_lossy().replace('/', "_");
    Some(crate::resolver::state_dir().join(format!("model-cwd-{key}")))
}

/// `std::env::var` trimmed to a non-empty `String`, else `None`.
fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Whole days from `from` to `to`, both `YYYY-MM-DD`. `None` if either fails to parse.
/// Positive when `to` is later. No timezone/DST concerns — pure civil-date arithmetic.
pub fn days_since(from: &str, to: &str) -> Option<i64> {
    Some(days_from_civil(to)? - days_from_civil(from)?)
}

/// Non-negative whole days elapsed from `from` to `to` (both `YYYY-MM-DD`) — the staleness/idle
/// measure used across doctor, lifecycle, metrics, next and reports. Unparseable or future
/// `from` clamps to 0.
pub fn idle_days_since(from: &str, to: &str) -> i64 {
    days_since(from, to).unwrap_or(0).max(0)
}

/// `YYYY-MM-DD` shape check (digits with dashes at positions 4 and 7). Loose: not
/// calendar-validated — enough to tell a date stem from a prose line or another filename.
pub fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// Days since the civil epoch (1970-01-01) for a `YYYY-MM-DD` string. `None` if it doesn't
/// parse. Public wrapper over the internal algorithm, for date math in callers.
pub fn epoch_day(date: &str) -> Option<i64> {
    days_from_civil(date)
}

/// Inverse of [`epoch_day`]: the `YYYY-MM-DD` date `z` days after the civil epoch (Howard
/// Hinnant's `civil_from_days`). Handles any integer `z` (negative = before 1970). Internal —
/// callers use [`add_days`].
fn date_from_epoch_day(z: i64) -> String {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// The `YYYY-MM-DD` date `n` days after `date` (negative `n` goes back). `None` if `date`
/// doesn't parse.
pub fn add_days(date: &str, n: i64) -> Option<String> {
    Some(date_from_epoch_day(epoch_day(date)? + n))
}

/// Resolve a scheduling target from the mutually-exclusive `--on <date>` / `--in <days>`
/// flags shared by the daily CLI and MCP surfaces. `Ok(None)` means neither was given (act on
/// today); `--on` is passed through verbatim for the daily layer to calendar-validate. `Err`
/// carries a human-readable message that each caller wraps in its own error type.
pub fn resolve_schedule_target(
    today: &str,
    on: Option<String>,
    in_days: Option<i64>,
) -> Result<Option<String>, String> {
    match (on, in_days) {
        (Some(_), Some(_)) => Err("specify a target date or an offset, not both".into()),
        (Some(d), None) => Ok(Some(d)),
        (None, Some(n)) => add_days(today, n)
            .map(Some)
            .ok_or_else(|| format!("can't compute a date {n} days from {today}")),
        (None, None) => Ok(None),
    }
}

/// Day of week for a `YYYY-MM-DD` date as `0=Sun .. 6=Sat`. `None` if it doesn't parse.
/// 1970-01-01 is a Thursday (epoch_day 0), so the offset is `+4`.
pub fn weekday_index(date: &str) -> Option<u32> {
    Some(((epoch_day(date)? + 4).rem_euclid(7)) as u32)
}

/// Days since the civil epoch (1970-01-01) for a `YYYY-MM-DD` string, via Howard Hinnant's
/// algorithm. Used for date diffs (staleness, metric windows) without a date dependency.
fn days_from_civil(date: &str) -> Option<i64> {
    let mut parts = date.trim().splitn(3, '-');
    let y0: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y0 - 1 } else { y0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let z = era * 146097 + doe - 719468;
    // Reject impossible calendar dates (e.g. 2026-02-30, 2025-04-31): the range check above
    // lets day 29-31 through for short months, where Hinnant's algorithm silently rolls over
    // into the next month. A real date round-trips to itself.
    if date_from_epoch_day(z) != format!("{y0:04}-{m:02}-{d:02}") {
        return None;
    }
    Some(z)
}

/// Run a non-fatal side effect, logging its failure instead of swallowing it. On `Err` it writes
/// one `bn: <context>: <err>` line to stderr so a dropped telemetry, paging, cache, or metrics
/// write leaves a trace; the `Ok` value is discarded either way.
///
/// Use this — never a bare `let _ =` — wherever the command must still succeed but a silent
/// failure would hide a real problem (a lost `bn staff` board row, an operator page that never
/// fired, a cache that stops warming). It is *not* for load-bearing writes a caller needs to
/// observe: propagate those. `context` should name the write, e.g. `"staff: staff_run event"`.
pub fn best_effort<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) {
    if let Err(e) = result {
        eprintln!("bn: {context}: {e}");
    }
}
