//! `bn staff feed --pick` — the prefix-B briefing popup (#265). The sibling of the attention
//! popup (`bn doctor --pick`, prefix C-a) and the fleet picker (`bn fleet pick`): an fzf popup over
//! the staff's briefing cards, unread-first then newest-first. `↵` renders the focused card in the
//! pager and marks it read; `m` marks read without opening; `ctrl-o` opens the card's link via the
//! shared opener (the same one prefix-N/`bn jump` uses); `ctrl-r` reloads.
//!
//! Read-marks round-trip through the same `bn staff feed` read-mark store (`staff::mark_read`), so
//! tmux and the nvim cockpit agree on what's unread — `bn staff feed` is the single source of
//! truth. Mirrors `picker.rs`'s self-referencing shape: this binary renders the row list, the
//! preview, and each per-key action; fzf's binds just re-invoke `bn` with a hidden `--pick-stage`.

use bn_core::resolver::Config;
use bn_core::staff::{self, BriefingCard, CardRef};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// The hidden callback stages a `--pick` fzf session re-invokes
/// (`bn staff feed --pick-stage <s> --pick-col …`).
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum StaffPickStage {
    /// Print the row list (the initial feed, or an fzf `reload` after a mark).
    List,
    /// Render the focused card into the preview panel (read-only — landing on a row never marks).
    Preview,
    /// Render the focused card in the pager and mark it read (the `↵` action).
    Open,
    /// Mark the focused card read without opening it (the `m` action).
    Mark,
    /// Open the focused card's link via the shared opener (the `ctrl-o` action).
    Link,
    /// Up-rate the focused card (the `ctrl-u` action): record a ▲ reaction and mark it read.
    RateUp,
    /// Down-rate the focused card (the `ctrl-d` action): record a ▼ reaction and mark it read.
    RateDown,
}

/// fzf field separator. Field 1 is the colored display; the hidden lookup columns follow.
const TAB: char = '\t';

/// A single briefing row: what fzf shows, plus the hidden columns its bindings reference.
struct Row {
    display: String,
    date: String,
    agent: String,
    path: PathBuf,
}

impl Row {
    /// `display \t date \t agent \t path` — the exact line fed to fzf and re-emitted by the `list`
    /// stage, so a `reload` reproduces the same columns.
    fn line(&self) -> String {
        format!(
            "{}{TAB}{}{TAB}{}{TAB}{}",
            self.display,
            self.date,
            self.agent,
            self.path.display()
        )
    }
}

/// Map a card-ref to its display row. Unread cards lead with a bright dot and a bold headline; read
/// ones sit dim — the "silence is the default state" grammar, and the same unread-dot idiom the
/// STAFF cockpit panel (#264) uses.
fn row_for(c: &CardRef) -> Row {
    let (dot, lead) = if c.read {
        (' ', "\x1b[2m")
    } else {
        ('\u{25cf}', "\x1b[1m")
    };
    Row {
        display: format!(
            "{lead}{dot} {}\x1b[0m  \x1b[36m{:<16}\x1b[0m {lead}{}\x1b[0m",
            c.date, c.agent, c.headline
        ),
        date: c.date.clone(),
        agent: c.agent.clone(),
        path: c.path.clone(),
    }
}

/// Header line: the card tally with an unread count, then the key legend — the same shape as the
/// attention popup's header.
fn header(cards: &[CardRef]) -> String {
    let unread = cards.iter().filter(|c| !c.read).count();
    format!(
        "\u{1f4ee} briefings · {} card{} ({unread} unread) · \u{21b5} read · ctrl-u \u{25b2} · ctrl-d \u{25bc} · m mark · ctrl-o link · ctrl-r reload",
        cards.len(),
        if cards.len() == 1 { "" } else { "s" }
    )
}

/// `bn staff feed --pick` — the interactive fzf briefing popup. Falls back to the plain `bn staff
/// feed` index when fzf is absent, and holds the popup open on the empty / no-fzf paths so the
/// message reads before the `-E` popup tears down.
pub fn run(cfg: &Config) {
    let cards = staff::list_cards(cfg);
    if cards.is_empty() {
        println!("No briefing cards yet — staff agents write one per run (see `bn staff`).");
        crate::picker::hold();
        return;
    }

    let rows: Vec<Row> = cards.iter().map(row_for).collect();
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let bn = crate::shq(&exe.display().to_string());
    let head = header(&cards);

    // {2}=date {3}=agent {4}=path, passed to every stage as ordered `--pick-col`s. Enter renders +
    // marks then reloads so the dot updates in place; m marks silently + reloads; ctrl-u/ctrl-d
    // up/down-rate (each marks read) + reload; ctrl-o opens the link; ctrl-r reloads. `execute`
    // keeps the popup up (like the bash originals) — Esc closes.
    let cols = "--pick-col {2} --pick-col {3} --pick-col {4}";
    let mut child = match Command::new("fzf")
        .arg("--ansi")
        .arg("--reverse")
        .arg("--with-nth=1")
        .arg("--delimiter=\t")
        .arg(format!(
            "--preview={bn} staff feed --pick-stage preview {cols}"
        ))
        .arg("--preview-window=right:60%:wrap")
        .arg(format!("--header={head}"))
        .arg(format!(
            "--bind=enter:execute({bn} staff feed --pick-stage open {cols})+reload({bn} staff feed --pick-stage list)"
        ))
        .arg(format!(
            "--bind=m:execute-silent({bn} staff feed --pick-stage mark {cols})+reload({bn} staff feed --pick-stage list)"
        ))
        .arg(format!(
            "--bind=ctrl-u:execute-silent({bn} staff feed --pick-stage rate-up {cols})+reload({bn} staff feed --pick-stage list)"
        ))
        .arg(format!(
            "--bind=ctrl-d:execute-silent({bn} staff feed --pick-stage rate-down {cols})+reload({bn} staff feed --pick-stage list)"
        ))
        .arg(format!(
            "--bind=ctrl-o:execute-silent({bn} staff feed --pick-stage link {cols})"
        ))
        .arg(format!(
            "--bind=ctrl-r:reload({bn} staff feed --pick-stage list)"
        ))
        .stdin(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => {
            // No fzf on PATH — degrade to the plain index, like the bash fallback.
            crate::emit_staff_feed(cfg, None, false);
            crate::picker::hold();
            return;
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let body = rows.iter().map(Row::line).collect::<Vec<_>>().join("\n");
        let _ = stdin.write_all(body.as_bytes());
    }
    let _ = child.wait();
}

/// Dispatch a hidden `--pick-stage` callback. `cols` are the focused row's hidden columns as fzf
/// passed them: `[date, agent, path]`.
pub fn stage(cfg: &Config, stage: StaffPickStage, cols: &[String]) {
    let f = |i: usize| cols.get(i).map(String::as_str).unwrap_or("");
    let (date, agent, path) = (f(0), f(1), f(2));
    match stage {
        StaffPickStage::List => {
            for c in staff::list_cards(cfg) {
                println!("{}", row_for(&c).line());
            }
        }
        StaffPickStage::Preview => preview(path),
        StaffPickStage::Open => {
            // Render first (the read), then mark — matching "↵ render the focused card and mark
            // read". Marking is idempotent, so the reload afterwards just refreshes the dot.
            if let Some(card) = read_card_at(path) {
                crate::picker::page(staff::render_card_md(&card).as_bytes());
            }
            mark(date, agent);
        }
        StaffPickStage::Mark => mark(date, agent),
        StaffPickStage::Link => {
            if let Some(link) = read_card_at(path).and_then(|c| card_link(&c)) {
                crate::open_url(&link);
            }
        }
        StaffPickStage::RateUp => rate(date, agent, staff::Rating::Up),
        StaffPickStage::RateDown => rate(date, agent, staff::Rating::Down),
    }
}

/// Detail panel for the focused row (the `preview` stage): the card's rendered markdown, read-only.
fn preview(path: &str) {
    match read_card_at(path) {
        Some(card) => print!("{}", staff::render_card_md(&card)),
        None => println!("(card unavailable)"),
    }
}

/// The first card item carrying a URL — the card's "link" for `ctrl-o`, opened through the same
/// [`crate::open_url`] prefix-N/`bn jump` use. `None` when the card links nothing.
fn card_link(card: &BriefingCard) -> Option<String> {
    card.items
        .iter()
        .map(|i| i.link.trim())
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

fn read_card_at(path: &str) -> Option<BriefingCard> {
    if path.is_empty() {
        return None;
    }
    staff::read_card(std::path::Path::new(path))
}

fn mark(date: &str, agent: &str) {
    if !date.is_empty() && !agent.is_empty() {
        let _ = staff::mark_read(date, agent);
    }
}

/// Record a ▲/▼ reaction to the focused card and mark it read — the `ctrl-u`/`ctrl-d` actions.
/// The note is empty (the popup is one-keystroke; notes come from the CLI `--note`). Best-effort
/// like [`mark`]: an fzf callback never surfaces an error, and the reload just refreshes the dot.
fn rate(date: &str, agent: &str, rating: staff::Rating) {
    if !date.is_empty() && !agent.is_empty() {
        let _ = staff::rate_card(date, agent, rating, "");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bn_core::staff::CardItem;
    use std::path::PathBuf;

    fn cref(agent: &str, date: &str, headline: &str, read: bool) -> CardRef {
        CardRef {
            agent: agent.into(),
            date: date.into(),
            headline: headline.into(),
            read,
            path: PathBuf::from(format!("/feed/{date}/{agent}.json")),
        }
    }

    #[test]
    fn row_line_has_four_columns_display_then_hidden() {
        let line = row_for(&cref("mentor", "2026-07-06", "One lesson", false)).line();
        let cols: Vec<&str> = line.split(TAB).collect();
        assert_eq!(cols.len(), 4, "display + date + agent + path");
        assert_eq!(cols[1], "2026-07-06");
        assert_eq!(cols[2], "mentor");
        assert_eq!(cols[3], "/feed/2026-07-06/mentor.json");
    }

    #[test]
    fn unread_row_is_bold_with_a_dot_read_row_is_dim() {
        let unread = row_for(&cref("chief", "2026-07-06", "Quiet day", false)).display;
        assert!(unread.starts_with("\x1b[1m\u{25cf} "), "got: {unread:?}");
        let read = row_for(&cref("archivist", "2026-07-03", "Week 27", true)).display;
        assert!(
            read.starts_with("\x1b[2m  "),
            "read leads dim, no dot: {read:?}"
        );
    }

    #[test]
    fn header_counts_unread_and_lists_keys() {
        let cards = vec![
            cref("chief", "2026-07-06", "a", false),
            cref("mentor", "2026-07-06", "b", false),
            cref("scout", "2026-07-04", "c", true),
        ];
        let h = header(&cards);
        assert!(h.contains("3 cards (2 unread)"), "got: {h}");
        assert!(h.contains("m mark") && h.contains("ctrl-o link") && h.contains("ctrl-r reload"));
        assert!(
            h.contains("ctrl-u") && h.contains("ctrl-d"),
            "rate keys in legend: {h}"
        );
    }

    #[test]
    fn card_link_is_first_item_url_or_none() {
        let mut card = BriefingCard {
            agent: "s".into(),
            date: "2026-07-06".into(),
            headline: "h".into(),
            body_md: String::new(),
            items: vec![],
        };
        assert_eq!(card_link(&card), None, "no items → no link");
        card.items.push(CardItem {
            kind: "note-append".into(),
            severity: "info".into(),
            title: "no link".into(),
            link: String::new(),
            repo: String::new(),
            branch: String::new(),
        });
        card.items.push(CardItem {
            kind: "attn".into(),
            severity: "urgent".into(),
            title: "cve".into(),
            link: "https://example.com/cve".into(),
            repo: String::new(),
            branch: String::new(),
        });
        assert_eq!(card_link(&card).as_deref(), Some("https://example.com/cve"));
    }

    #[test]
    fn read_card_at_empty_path_is_none() {
        assert!(read_card_at("").is_none());
    }

    #[test]
    fn rate_stage_records_reaction_and_marks_read() {
        bn_core::test_support::with_state_dir_path(|p| {
            let cfg = bn_core::test_support::config(p);
            // cols = [date, agent, path] as fzf hands them to the callback; path is unused by rate.
            let cols = vec![
                "2026-07-06".to_string(),
                "mentor".to_string(),
                String::new(),
            ];
            stage(&cfg, StaffPickStage::RateDown, &cols);
            let ratings = staff::read_ratings();
            assert_eq!(ratings.len(), 1);
            assert_eq!(ratings["2026-07-06/mentor"].rating, staff::Rating::Down);
            // Rating marks the card read — you can't react to a card you never saw.
            assert!(
                staff::read_marks().contains("2026-07-06/mentor"),
                "rating marks read"
            );
        });
    }

    #[test]
    fn rate_stage_ignores_empty_selector() {
        bn_core::test_support::with_state_dir_path(|p| {
            let cfg = bn_core::test_support::config(p);
            stage(
                &cfg,
                StaffPickStage::RateUp,
                &[String::new(), String::new()],
            );
            assert!(
                staff::read_ratings().is_empty(),
                "no columns → nothing rated"
            );
        });
    }
}
