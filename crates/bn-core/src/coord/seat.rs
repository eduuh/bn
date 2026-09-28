//! Seat addresses: `repo:branch` for an agent, `human:<name>` for a person.

use super::{CoordError, Result};

const HUMAN: &str = "human:";

/// True for a `human:<name>` seat.
pub fn is_human(seat: &str) -> bool {
    seat.starts_with(HUMAN)
}

/// Normalize a seat the way the resolver normalizes a target, so `bn:feat/x` and `bn:feat-x`
/// name one seat: repo lowercased, `/` in the branch folded to `-`; a human's name lowercased.
/// Refuses anything that is neither shape.
pub fn normalize(seat: &str) -> Result<String> {
    let seat = seat.trim();
    let Some((head, tail)) = seat.split_once(':') else {
        return Err(invalid(seat));
    };
    let (head, tail) = (head.trim(), tail.trim());
    if head.is_empty() || tail.is_empty() || tail.contains(':') {
        return Err(invalid(seat));
    }
    if head.eq_ignore_ascii_case("human") {
        return Ok(format!("{HUMAN}{}", tail.to_ascii_lowercase()));
    }
    Ok(format!(
        "{}:{}",
        head.to_ascii_lowercase(),
        tail.replace('/', "-")
    ))
}

fn invalid(seat: &str) -> CoordError {
    CoordError::Invalid(format!(
        "'{seat}' is not a seat — use repo:branch for an agent or human:<name> for a person"
    ))
}

/// The seat a command acts as. The first of: an explicit `--as`, `$BN_SEAT`, the `repo:branch`
/// of the worktree the command runs in, then the person at the keyboard (`human:$USER`). Each
/// candidate is normalized; an explicit one that doesn't parse is an error rather than a silent
/// fall-through, so a typo never makes you act as someone else.
pub fn acting(
    flag: Option<&str>,
    env: Option<&str>,
    here: Option<&str>,
    user: &str,
) -> Result<String> {
    if let Some(s) = explicit(flag).or_else(|| explicit(env)) {
        return normalize(s);
    }
    if let Some(s) = here {
        return normalize(s);
    }
    let user = user.trim();
    normalize(&format!(
        "{HUMAN}{}",
        if user.is_empty() { "me" } else { user }
    ))
}

/// Split an agent seat into `(repo, branch)`; `None` for a human seat.
pub fn agent_target(seat: &str) -> Option<(&str, &str)> {
    if is_human(seat) {
        return None;
    }
    seat.split_once(':')
}

fn explicit(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}
