//! `bn queue timeline` — the day, as an axis.
//!
//! The terminal half of the timeline. It builds the *same* `Timeline` struct the web view renders,
//! so the two cannot disagree about what is happening today; only the drawing differs.
//!
//! Width-responsive like `bn pipeline`: an hour per column when there is room, two-hour buckets
//! when there isn't. Below that a Gantt is unreadable and pretending otherwise is worse than a
//! list, so the narrow form is the density strip plus counts.
use crate::*;
use bn_core::queue::timeline::{self, BandId, Timeline};

/// `bn queue timeline` — render today (or `--date`).
pub(crate) fn emit_timeline(cfg: &Config, date: Option<&str>, json: bool) {
    use bn_core::clock::Clock;
    let now = bn_core::clock::RealClock.now_epoch_secs();
    let date = match date {
        Some(raw) if timeline::parse_date(raw).is_none() => {
            fail(&format!("bn queue timeline: --date must be YYYY-MM-DD, got '{raw}'"))
        }
        Some(raw) => raw.trim().to_string(),
        None => bn_core::util::local_date_of(now),
    };
    let tl = timeline::build_live(cfg, &date, now)
        .unwrap_or_else(|error| fail(&format!("bn queue timeline: invalid agent catalog: {error}")));
    if json {
        println!(
            "{}",
            Envelope::success(
                "queue-timeline",
                serde_json::to_value(&tl).unwrap_or_default()
            )
            .to_json()
        );
        return;
    }
    print!("{}", render(&tl, terminal_width()));
}

fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|w| *w >= 20)
        .unwrap_or(100)
}

/// Render at `width`: an hour per column when there is room, two-hour buckets below.
fn render(tl: &Timeline, width: usize) -> String {
    let cols: usize = if width >= 100 { 24 } else { 12 };
    let per = 24 / cols; // hours per column
    let mut out = String::new();
    out.push_str(&format!("\u{f073} {} — queue timeline\n", tl.date));
    if !tl.pauses.is_empty() {
        for p in &tl.pauses {
            let why = if p.reason.is_empty() {
                String::new()
            } else {
                format!(" — {}", p.reason)
            };
            out.push_str(&format!("\u{f05e} paused: {}{why}\n", p.scope.key()));
        }
    }
    out.push('\n');
    out.push_str(&axis(cols, per));
    for band in &tl.bands {
        if band.marks.is_empty() {
            continue;
        }
        out.push_str(&row(band.label.as_str(), &lanes(tl, band.id, cols, per)));
    }
    out.push('\n');
    out.push_str(&format!("  now {} \u{00b7} \u{25b0} running  \u{25c6} scheduled  \u{25c7} projected  \u{2717} blocked\n", bn_core::util::clock_hm(tl.now)));
    if tl.beyond > 0 {
        out.push_str(&format!(
            "  {} entr{} beyond the horizon at projected throughput\n",
            tl.beyond,
            if tl.beyond == 1 { "y" } else { "ies" }
        ));
    }
    out
}

/// The hour ruler: a tick per column, a bar every six hours, with a label row beneath.
///
/// One column is one character, so an hour number cannot be printed *in* the column. A separate
/// aligned label row is the honest way to say which hour a mark sits in — cramming `h / 10` into
/// the tick row produced `0.....0.....1.....1`, which reads as data and is not.
fn axis(cols: usize, per: usize) -> String {
    let major = (6 / per).max(1);
    let ticks: String = (0..cols)
        .map(|c| if c.is_multiple_of(major) { '|' } else { '.' })
        .collect();
    let mut labels = vec![' '; cols];
    for c in (0..cols).step_by(major) {
        for (i, ch) in format!("{:02}", c * per).chars().enumerate() {
            if c + i < cols {
                labels[c + i] = ch;
            }
        }
    }
    format!(
        "{:<11}{ticks}\n{:<11}{}\n",
        "",
        "",
        labels.iter().collect::<String>()
    )
}

/// One band's row of glyphs.
fn lanes(tl: &Timeline, band: BandId, cols: usize, per: usize) -> Vec<char> {
    let mut cells = vec![' '; cols];
    let Some(b) = tl.bands.iter().find(|b| b.id == band) else {
        return cells;
    };
    for mark in &b.marks {
        let hour = ((mark.at - tl.start) / 3600).clamp(0, 23) as usize;
        let col = (hour / per).min(cols - 1);
        // A blocked mark outranks everything: the reason it is not moving is the thing to see.
        cells[col] = match (mark.blocked, band, cells[col]) {
            (true, _, _) => '✗',
            (_, _, '✗') => '✗',
            (_, BandId::Running, _) => '▰',
            (_, BandId::Scheduled, _) => '◆',
            (_, BandId::Projected, _) => '◇',
            (_, BandId::Done, _) => '·',
        };
    }
    cells
}

fn row(label: &str, cells: &[char]) -> String {
    format!("{:<11}{}\n", label, cells.iter().collect::<String>())
}
