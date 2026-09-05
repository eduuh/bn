//! Priority windows — work that should be preferred at particular times of day.
//!
//! A queue orders work by importance, but importance is not constant. Warming repos, running the
//! heavy nightly sweeps, letting a long scout loose: none of that should compete with you at
//! 10am, and all of it is exactly what the machine should be doing while you are at lunch or after
//! you close the laptop.
//!
//! A window is a recurring span plus a priority delta. While it is open, matching work ranks
//! higher — it is preferred, not forced, so capacity, skills and conflicts still apply. Nothing is
//! *scheduled* by a window; that is what a cron's `on_calendar` is for. This answers the different
//! question of what the machine should reach for first when it has a free hand.
//!
//! The window's own opening is an `OnCalendar` spec, so it uses the same schedule vocabulary as
//! everything else in bn rather than inventing a second one.
use crate::backlog::BacklogEntry;
use crate::queue::classify::{self, QueueMode};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const POOL_WARM_LUNCH: &str = "pool-warm-lunch";
pub const POOL_WARM_AFTER_WORK: &str = "pool-warm-after-work";
const POOL_WARM_MATCH: &str = "source:cron:pool-warm";

/// A recurring span during which some work is preferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriorityWindow {
    /// What to call it in `bn queue status` — `lunch`, `evening`, `overnight`.
    pub name: String,
    /// When it opens, as an `OnCalendar` spec: `Mon..Fri *-*-* 12:30:00`.
    pub at: String,
    /// How long it stays open, in seconds.
    pub secs: i64,
    /// How much to raise matching work's priority while open.
    ///
    /// A delta rather than an absolute, so relative order *within* the boosted set is preserved —
    /// a window says "prefer this class of work now", not "make it all equally urgent".
    pub boost: i64,
    /// What it applies to: `kind:<k>`, `repo:<r>`, `source:<prefix>`, or `*` for everything.
    pub matches: Vec<String>,
    /// How the window participates in presence policy.
    #[serde(default)]
    pub mode: WindowMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum WindowMode {
    /// Pure priority boost when open.
    #[default]
    Prefer,
    /// User-supervised time: foreground work receives the window boost.
    Watched,
    /// Away/off-peak time: matching opportunistic work may run.
    Away,
    /// A maintenance deadline: matching work becomes mandatory once the window opens.
    Deadline,
}

impl WindowMode {
    pub fn parse(s: &str) -> Option<WindowMode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "prefer" | "priority" => Some(WindowMode::Prefer),
            "watched" | "watch" | "foreground" => Some(WindowMode::Watched),
            "away" | "off-peak" | "opportunistic" => Some(WindowMode::Away),
            "deadline" | "mandatory" => Some(WindowMode::Deadline),
            _ => None,
        }
    }
}

impl PriorityWindow {
    /// Whether the window is open at `now`.
    ///
    /// Open means: its most recent firing is within `secs` of now. Reusing
    /// [`crate::staff_schedule::previous_occurrence`] means a window inherits every schedule shape
    /// bn already understands — weekdays, hour steps, concrete dates — instead of a parallel
    /// grammar that would drift from it.
    pub fn is_open(&self, now: i64) -> bool {
        crate::staff_schedule::previous_occurrence(&self.at, now)
            .is_some_and(|start| now >= start && now < start.saturating_add(self.secs.max(0)))
    }

    /// Whether this window applies to `entry`.
    ///
    /// `source:` matches by prefix, so `source:cron:` covers every scheduled job without listing
    /// them. `class:` and `subtype:` match the derived queue class.
    pub fn applies_to(&self, entry: &BacklogEntry) -> bool {
        self.matches.iter().any(|m| {
            let m = m.trim();
            if m == "*" {
                return true;
            }
            let class = classify::classify(entry);
            match m.split_once(':') {
                Some(("kind", k)) => entry.kind.label().eq_ignore_ascii_case(k),
                Some(("repo", r)) => entry.repo.eq_ignore_ascii_case(r),
                Some(("source", p)) => entry.source.starts_with(p),
                Some(("class", c)) => class.mode.label().eq_ignore_ascii_case(c),
                Some(("subtype", s)) => class.subtype.label().eq_ignore_ascii_case(s),
                _ => false,
            }
        })
    }

    /// Seconds until this window next closes, for the status line. `None` when it is not open.
    pub fn closes_in(&self, now: i64) -> Option<i64> {
        crate::staff_schedule::previous_occurrence(&self.at, now)
            .map(|start| start.saturating_add(self.secs.max(0)))
            .filter(|end| *end > now)
            .map(|end| end - now)
    }
}

/// The total boost applying to `entry` at `now`.
///
/// Summed rather than maxed: overlapping windows are a deliberate statement — "it is both lunch
/// *and* a Friday" — and the arithmetic should say what the configuration says. Pure, so the
/// effective priority of any entry is reproducible from a captured snapshot.
pub fn boost_for(entry: &BacklogEntry, windows: &[PriorityWindow], now: i64) -> i64 {
    windows
        .iter()
        .filter(|w| {
            if !w.is_open(now) {
                return false;
            }
            match w.mode {
                WindowMode::Prefer => w.applies_to(entry),
                WindowMode::Watched => classify::classify(entry).mode == QueueMode::Foreground,
                WindowMode::Away | WindowMode::Deadline => w.applies_to(entry),
            }
        })
        .fold(0i64, |acc, w| acc.saturating_add(w.boost))
}

/// When the machine is next claimed, and by which window.
///
/// Only [`WindowMode::Deadline`] windows claim it. A `Prefer` or `Away` window changes what the
/// machine reaches for first; a `Deadline` window is the one that says "this must happen now",
/// which is the only kind that makes starting something long beforehand a mistake.
///
/// Deliberately about the *machine*, not about you. A meeting bounds your attention, and
/// `bn next --fit` already sizes your plan against it; a cron finishing during your standup is
/// nobody's problem. Conflating the two would have the dispatcher idle because you are busy.
pub fn next_claim(windows: &[PriorityWindow], now: i64) -> Option<(i64, String)> {
    windows
        .iter()
        .filter(|w| w.mode == WindowMode::Deadline && !w.is_open(now))
        .filter_map(|w| {
            crate::staff_schedule::next_occurrence(&w.at, now).map(|at| (at, w.name.clone()))
        })
        .min_by_key(|(at, _)| *at)
}

/// The windows open at `now`, for the status surface.
pub fn open(windows: &[PriorityWindow], now: i64) -> Vec<&PriorityWindow> {
    windows.iter().filter(|w| w.is_open(now)).collect()
}

/// Whether this entry may run as opportunistic work at `now`.
pub fn allows_opportunistic(entry: &BacklogEntry, windows: &[PriorityWindow], now: i64) -> bool {
    windows.iter().any(|w| {
        matches!(w.mode, WindowMode::Away | WindowMode::Deadline)
            && w.is_open(now)
            && w.applies_to(entry)
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolWarmWindow {
    pub name: String,
    pub at: String,
    pub fore: String,
    pub mode: WindowMode,
}

pub fn pool_warm_windows(windows: &[PriorityWindow]) -> Vec<PoolWarmWindow> {
    windows
        .iter()
        .filter(|w| {
            matches!(w.mode, WindowMode::Away | WindowMode::Deadline)
                && w.matches.iter().any(|m| m == POOL_WARM_MATCH)
        })
        .map(|w| PoolWarmWindow {
            name: w.name.clone(),
            at: w.at.clone(),
            fore: format_duration(w.secs),
            mode: w.mode,
        })
        .collect()
}

pub fn default_pool_warm_priority_windows() -> Vec<PriorityWindow> {
    vec![
        priority_window_from_pool(PoolWarmWindow {
            name: POOL_WARM_LUNCH.to_string(),
            at: "Mon..Fri *-*-* 12:30:00".to_string(),
            fore: "90m".to_string(),
            mode: WindowMode::Away,
        }),
        priority_window_from_pool(PoolWarmWindow {
            name: POOL_WARM_AFTER_WORK.to_string(),
            at: "*-*-* 17:30:00".to_string(),
            fore: "2h".to_string(),
            mode: WindowMode::Deadline,
        }),
    ]
}

fn priority_window_from_pool(win: PoolWarmWindow) -> PriorityWindow {
    PriorityWindow {
        name: win.name,
        at: win.at,
        secs: crate::focus::parse_duration(&win.fore).unwrap_or(90 * 60),
        boost: 100,
        matches: vec![POOL_WARM_MATCH.to_string()],
        mode: win.mode,
    }
}

/// Upgrade the two built-in pool windows without changing their configured clock times.
///
/// Existing configs wrote both windows as weekday `away` windows. The names are stable, so they
/// provide a migration seam: lunch remains opportunistic, while after-work becomes a seven-day
/// deadline using the time already chosen on this machine.
pub fn normalize_pool_warm_priority_windows(
    mut windows: Vec<PriorityWindow>,
) -> Vec<PriorityWindow> {
    for window in &mut windows {
        match window.name.as_str() {
            POOL_WARM_LUNCH => window.mode = WindowMode::Away,
            POOL_WARM_AFTER_WORK => {
                window.mode = WindowMode::Deadline;
                if let Some(time) = window.at.split_whitespace().last() {
                    window.at = format!("*-*-* {time}");
                }
            }
            _ => {}
        }
    }
    windows
}

pub fn read_pool_warm_windows(
    config_path: &Path,
    fallback: &[PriorityWindow],
) -> Vec<PoolWarmWindow> {
    std::fs::read_to_string(config_path)
        .ok()
        .map(|text| pool_warm_windows_from_config(&text))
        .filter(|wins| !wins.is_empty())
        .unwrap_or_else(|| pool_warm_windows(fallback))
}

pub fn write_pool_warm_windows(
    config_path: &Path,
    windows: &[PoolWarmWindow],
) -> std::io::Result<()> {
    let current = std::fs::read_to_string(config_path).unwrap_or_default();
    let next = edit_pool_warm_windows(&current, windows);
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::store::write_atomic(config_path, next.as_bytes())
}

fn pool_warm_windows_from_config(text: &str) -> Vec<PoolWarmWindow> {
    let mut out = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if line.trim() != "[[queue.windows]]" {
            continue;
        }
        let mut block = Vec::new();
        while let Some(next) = lines.peek() {
            if next.trim_start().starts_with("[[") || next.trim_start().starts_with('[') {
                break;
            }
            block.push(lines.next().unwrap_or_default());
        }
        let Some(name) = string_value(&block, "name") else {
            continue;
        };
        if !matches!(
            Some(name.as_str()),
            Some(POOL_WARM_LUNCH) | Some(POOL_WARM_AFTER_WORK)
        ) {
            continue;
        }
        let Some(at) = string_value(&block, "at") else {
            continue;
        };
        let fore = string_value(&block, "fore").unwrap_or_else(|| "1h".to_string());
        let mode = string_value(&block, "mode")
            .as_deref()
            .and_then(WindowMode::parse)
            .unwrap_or(if name == POOL_WARM_AFTER_WORK {
                WindowMode::Deadline
            } else {
                WindowMode::Away
            });
        let at = if name == POOL_WARM_AFTER_WORK {
            at.split_whitespace()
                .last()
                .map(|time| format!("*-*-* {time}"))
                .unwrap_or(at)
        } else {
            at
        };
        out.push(PoolWarmWindow {
            name,
            at,
            fore,
            mode,
        });
    }
    out
}

pub fn edit_pool_warm_windows(content: &str, windows: &[PoolWarmWindow]) -> String {
    let mut out = Vec::new();
    let mut lines = content.lines().peekable();
    while let Some(line) = lines.next() {
        if line.trim() != "[[queue.windows]]" {
            out.push(line.to_string());
            continue;
        }
        let mut block = vec![line.to_string()];
        while let Some(next) = lines.peek() {
            if next.trim_start().starts_with("[[") || next.trim_start().starts_with('[') {
                break;
            }
            block.push(lines.next().unwrap_or_default().to_string());
        }
        let remove = string_value(
            &block.iter().map(String::as_str).collect::<Vec<_>>(),
            "name",
        )
        .is_some_and(|name| name == POOL_WARM_LUNCH || name == POOL_WARM_AFTER_WORK);
        if !remove {
            out.extend(block);
        }
    }
    while out.last().is_some_and(|line| line.trim().is_empty()) {
        out.pop();
    }
    for win in windows {
        out.push(String::new());
        out.push("[[queue.windows]]".to_string());
        out.push(format!("name = \"{}\"", escape_toml(&win.name)));
        out.push(format!("at = \"{}\"", escape_toml(&win.at)));
        out.push(format!("fore = \"{}\"", escape_toml(&win.fore)));
        let mode = match win.mode {
            WindowMode::Prefer => "prefer",
            WindowMode::Watched => "watched",
            WindowMode::Away => "away",
            WindowMode::Deadline => "deadline",
        };
        out.push(format!("mode = \"{mode}\""));
        out.push("boost = 100".to_string());
        out.push("matches = [\"source:cron:pool-warm\"]".to_string());
    }
    let mut text = out.join("\n");
    text.push('\n');
    text
}

fn string_value(lines: &[&str], key: &str) -> Option<String> {
    let prefix = format!("{key} = ");
    lines.iter().find_map(|line| {
        let raw = line.trim().strip_prefix(&prefix)?.trim();
        raw.strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .map(|s| s.replace("\\\"", "\""))
    })
}

fn escape_toml(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn format_duration(secs: i64) -> String {
    if secs % 3600 == 0 {
        format!("{}h", secs / 3600)
    } else if secs % 60 == 0 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}
