//! Which tmux keys actually get pressed: `bn run tmux-keylog` (the `tick-5m` job that records them)
//! and `bn tmux-keys` (the report that names the bindings nobody uses).
//!
//! tmux keeps no key history of its own beyond the message log, where every *bound* key press lands
//! as `HH:MM: <client> key <KEY>: <command>` — newest row first, no key table, bounded by
//! `message-limit` and gone on server restart. So an audit of "which features have I never used"
//! had to be inferred from side effects. The job drains that log into
//! `<state_dir>/tmux-keys.jsonl` every five minutes; the report joins it against
//! `list-keys -F`, whose `#{key_command}` prints the same form the log does, so a press files
//! under the exact binding (`[` previous-window vs copy-mode `[` previous-prompt) rather than just
//! its key name.
use crate::*;
use std::collections::HashMap;
use std::io::Write as _;

const LOG_FILE: &str = "tmux-keys.jsonl";
const CURSOR_FILE: &str = "tmux-keylog.cursor";
/// How many of the previous snapshot's newest rows the cursor keeps to find its place again. Rows
/// carry a client id that increments per tmux invocation, so a run of 50 does not recur by chance.
const TAIL_ROWS: usize = 50;
/// Tables a binding is matched in. `copy-mode` (emacs) is left out: this config sets vi keys.
const TABLES: [&str; 3] = ["prefix", "root", "copy-mode-vi"];
const BINDING_FORMAT: &str = "#{key_table}\t#{key_string}\t#{key_note}\t#{key_command}";

/// One key press as the message log records it.
#[derive(Debug, PartialEq)]
pub(crate) struct KeyRow<'a> {
    pub hhmm: &'a str,
    pub key: &'a str,
    pub cmd: &'a str,
}

/// Parse `HH:MM: <client> key <KEY>: <command>`. Keys never contain spaces, so the key is the
/// token after `key ` less its trailing colon — which is what keeps `::` (the `:` binding) intact.
pub(crate) fn parse_key_row(line: &str) -> Option<KeyRow<'_>> {
    let (hhmm, rest) = line.split_once(": ")?;
    let (_client, rest) = rest.split_once(' ')?;
    let rest = rest.strip_prefix("key ")?;
    let (key, cmd) = rest.split_once(' ')?;
    Some(KeyRow { hhmm, key: key.strip_suffix(':')?, cmd })
}

/// Where the last run stopped reading: the tmux server it read, and that snapshot's newest rows.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct Cursor {
    pub pid: String,
    pub tail: Vec<String>,
}

/// Index of the first row not yet recorded in `rows` (oldest first), and why rows may be missing.
/// A different server pid is a restart; a tail that no longer appears means the log rolled past
/// everything the last run saw. Both read the whole log and report the gap, so the report can say
/// its counts are a lower bound. No cursor at all is the first run: nothing was lost, only unseen.
pub(crate) fn unread_from(prev: Option<&Cursor>, pid: &str, rows: &[&str]) -> (usize, Option<&'static str>) {
    let Some(prev) = prev else {
        return (0, None);
    };
    if prev.pid != pid {
        return (0, Some("restart"));
    }
    if prev.tail.is_empty() {
        return (0, None);
    }
    let n = prev.tail.len();
    (n..=rows.len())
        .rev()
        .find(|&end| rows[end - n..end].iter().zip(&prev.tail).all(|(a, b)| a == b))
        .map_or((0, Some("rolled")), |end| (end, None))
}

/// The date a row happened on. The log keeps only `HH:MM`, so a row later in the day than now is
/// from yesterday (a run just after midnight reading rows from just before it).
pub(crate) fn row_date<'a>(hhmm: &str, now_hhmm: &str, today: &'a str, yesterday: &'a str) -> &'a str {
    if hhmm > now_hhmm {
        yesterday
    } else {
        today
    }
}

/// `bn run tmux-keylog`: append the key presses logged since the last run. Silent and successful
/// when no tmux server is running, because there is nothing to lose.
pub(crate) fn job_tmux_keylog(_cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let (Some(pid), Some(raw)) = (
        crate::proc::tmux(&["display-message", "-p", "#{pid}"]),
        crate::proc::tmux_raw(&["show-messages"]),
    ) else {
        return 0;
    };
    let dir = bn_core::resolver::state_dir();
    let cursor_path = dir.join(CURSOR_FILE);
    let prev: Option<Cursor> =
        std::fs::read_to_string(&cursor_path).ok().and_then(|s| serde_json::from_str(&s).ok());
    let rows: Vec<&str> = raw.lines().rev().collect();
    let (start, gap) = unread_from(prev.as_ref(), &pid, &rows);

    let now = bn_core::util::now_epoch_secs();
    let today = bn_core::util::local_date_of(now);
    let yesterday = bn_core::util::local_date_of(now - 86_400);
    let short = bn_core::util::fmt_local_short(now);
    let now_hhmm = short.get(6..).unwrap_or("23:59");

    let mut out = String::new();
    if let Some(gap) = gap {
        out.push_str(&serde_json::json!({"date": today, "hhmm": now_hhmm, "gap": gap}).to_string());
        out.push('\n');
    }
    for row in rows[start..].iter().filter_map(|l| parse_key_row(l)) {
        let date = row_date(row.hhmm, now_hhmm, &today, &yesterday);
        let line = serde_json::json!({"date": date, "hhmm": row.hhmm, "key": row.key, "cmd": row.cmd});
        out.push_str(&line.to_string());
        out.push('\n');
    }

    if std::fs::create_dir_all(&dir).is_err() {
        return 1;
    }
    if !out.is_empty() {
        let appended = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(LOG_FILE))
            .and_then(|mut f| f.write_all(out.as_bytes()));
        if appended.is_err() {
            return 1;
        }
    }
    let tail = rows[rows.len().saturating_sub(TAIL_ROWS)..].iter().map(|s| s.to_string()).collect();
    let cursor = serde_json::to_string(&Cursor { pid, tail }).unwrap_or_default();
    i32::from(std::fs::write(cursor_path, cursor).is_err())
}

/// One live binding from `list-keys -F BINDING_FORMAT`.
#[derive(Debug, PartialEq)]
pub(crate) struct Binding {
    pub table: String,
    pub key: String,
    pub note: String,
    pub cmd: String,
}

pub(crate) fn parse_bindings(raw: &str) -> Vec<Binding> {
    raw.lines()
        .filter_map(|line| {
            let mut f = line.splitn(4, '\t');
            let (table, key, note, cmd) = (f.next()?, f.next()?, f.next()?, f.next()?);
            TABLES.contains(&table).then(|| Binding {
                table: table.to_string(),
                key: key.to_string(),
                note: note.to_string(),
                cmd: cmd.to_string(),
            })
        })
        .collect()
}

/// The binding a logged press ran: the one whose key *and* command match, else the only binding
/// with that key (the config changed since the press, but the key is unambiguous), else none.
pub(crate) fn match_binding<'a>(bindings: &'a [Binding], key: &str, cmd: &str) -> Option<&'a Binding> {
    let same_key: Vec<&Binding> = bindings.iter().filter(|b| b.key == key).collect();
    same_key
        .iter()
        .find(|b| b.cmd == cmd)
        .or_else(|| same_key.first().filter(|_| same_key.len() == 1))
        .copied()
}

fn is_mouse(key: &str) -> bool {
    ["Mouse", "Wheel", "DoubleClick", "TripleClick", "SecondClick"].iter().any(|p| key.starts_with(p))
}

/// `bn tmux-keys [--since 7d]`: every curated binding (a `Section · description` note) in the
/// prefix and copy-mode tables with how often it was pressed, zeros included — the zeros are the
/// point.
pub(crate) fn emit_tmux_keys(since: Option<String>) {
    let secs = since.as_deref().map_or(Some(7 * 86_400), bn_core::focus::parse_duration);
    let Some(secs) = secs else {
        eprintln!("bn: --since wants a duration like 7d, 36h or 90m");
        std::process::exit(2);
    };
    let Some(raw) = crate::proc::tmux_raw(&["list-keys", "-F", BINDING_FORMAT]) else {
        eprintln!("bn: tmux is not reachable — no key table to read");
        std::process::exit(1);
    };
    let bindings = parse_bindings(&raw);
    let cutoff = bn_core::util::local_date_of(bn_core::util::now_epoch_secs() - secs);
    let log = std::fs::read_to_string(bn_core::resolver::state_dir().join(LOG_FILE)).unwrap_or_default();

    let mut counts: HashMap<(&str, &str), usize> = HashMap::new();
    let mut other: HashMap<String, usize> = HashMap::new();
    let (mut presses, mut mouse, mut gaps) = (0usize, 0usize, 0usize);
    for row in log.lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()) {
        if row["date"].as_str().is_none_or(|d| d < cutoff.as_str()) {
            continue;
        }
        if row.get("gap").is_some() {
            gaps += 1;
            continue;
        }
        let (key, cmd) = (row["key"].as_str().unwrap_or(""), row["cmd"].as_str().unwrap_or(""));
        presses += 1;
        match match_binding(&bindings, key, cmd) {
            _ if is_mouse(key) => mouse += 1,
            Some(b) => *counts.entry((b.table.as_str(), b.key.as_str())).or_default() += 1,
            None => *other.entry(key.to_string()).or_default() += 1,
        }
    }

    let ink = style::Ink::auto();
    let (hd, num, d, n) = (ink.open(style::Tone::Head), ink.open(style::Tone::Accent), ink.open(style::Tone::Meta), ink.close());
    let gap_note = if gaps > 0 { format!(" · {gaps} gap(s), counts are a floor") } else { String::new() };
    println!("{hd}tmux key usage{n}  {d}since {cutoff} · {presses} presses logged{gap_note}{n}");
    if log.is_empty() {
        println!("{d}No key log yet. `bn run tmux-keylog` records presses every 5 minutes (tick-5m).{n}");
    }

    let (mut curated, mut unused) = (0usize, 0usize);
    for (table, label) in [("prefix", ""), ("copy-mode-vi", " (in copy mode)")] {
        let noted: String = bindings
            .iter()
            .filter(|b| b.table == table && !b.note.is_empty())
            .map(|b| format!("{} {}\n", b.key, b.note))
            .collect();
        for (section, rows) in noted_key_sections(&noted, label) {
            if section.starts_with("Stock tmux") {
                continue;
            }
            println!("\n{hd}{section}{n}");
            for (key, desc) in rows {
                let count = counts.get(&(table, key.as_str())).copied().unwrap_or(0);
                curated += 1;
                if count == 0 {
                    unused += 1;
                    println!("  {d}{:>5}  {key:<10} {desc}{n}", "never");
                } else {
                    println!("  {num}{count:>5}{n}  {key:<10} {desc}");
                }
            }
        }
    }

    let mut other: Vec<_> = other.into_iter().collect();
    other.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let other: Vec<String> = other.iter().take(8).map(|(k, c)| format!("{k} {c}")).collect();
    println!("\n{d}mouse {mouse}{}{n}", if other.is_empty() { String::new() } else { format!(" · other keys: {}", other.join(", ")) });
    println!("{hd}{unused} of {curated} bindings never pressed{n}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor(pid: &str, tail: &[&str]) -> Cursor {
        Cursor { pid: pid.to_string(), tail: tail.iter().map(|s| s.to_string()).collect() }
    }

    /// Row shapes captured from tmux 3.7c driving a real client with `send-keys -K`.
    #[test]
    fn a_key_row_parses_and_everything_else_does_not() {
        assert_eq!(
            parse_key_row("08:28: /dev/pts/17 key [: send-keys -X previous-prompt"),
            Some(KeyRow { hhmm: "08:28", key: "[", cmd: "send-keys -X previous-prompt" })
        );
        assert_eq!(parse_key_row("08:28: client-1 key :: command-prompt").map(|r| r.key), Some(":"));
        assert_eq!(parse_key_row("08:28: /dev/pts/17 key \": split-window").map(|r| r.key), Some("\""));
        assert_eq!(parse_key_row("08:28: client-57416 command: bind-key -n C-BSpace next-window"), None);
        assert_eq!(parse_key_row("08:28: /dev/pts/17 message: hi"), None);
    }

    #[test]
    fn the_first_run_reads_everything_without_calling_it_a_gap() {
        assert_eq!(unread_from(None, "1", &["a", "b"]), (0, None));
    }

    #[test]
    fn an_overlapping_log_resumes_after_the_saved_tail() {
        let prev = cursor("1", &["b", "c"]);
        assert_eq!(unread_from(Some(&prev), "1", &["a", "b", "c", "d", "e"]), (3, None));
        assert_eq!(unread_from(Some(&prev), "1", &["b", "c"]), (2, None));
    }

    #[test]
    fn a_rolled_log_or_a_new_server_reads_everything_and_reports_the_gap() {
        let prev = cursor("1", &["b", "c"]);
        assert_eq!(unread_from(Some(&prev), "1", &["x", "y"]), (0, Some("rolled")));
        assert_eq!(unread_from(Some(&prev), "2", &["b", "c", "d"]), (0, Some("restart")));
    }

    #[test]
    fn a_row_later_than_now_happened_yesterday() {
        assert_eq!(row_date("23:58", "00:02", "2026-09-12", "2026-09-11"), "2026-09-11");
        assert_eq!(row_date("00:01", "00:02", "2026-09-12", "2026-09-11"), "2026-09-12");
    }

    const BINDINGS: &str = "prefix\t[\tWindows · previous window\tprevious-window\n\
                            copy-mode-vi\t[\tCopy mode · jump to the previous shell prompt\tsend-keys -X previous-prompt\n\
                            prefix\tg\tTools · lazygit\tdisplay-popup -E \"lazygit\"\n\
                            copy-mode\t[\t\tsend-keys -X previous-paragraph\n";

    #[test]
    fn bindings_outside_the_matched_tables_are_dropped() {
        let b = parse_bindings(BINDINGS);
        assert_eq!(b.len(), 3);
        assert_eq!(b[2], Binding {
            table: "prefix".into(),
            key: "g".into(),
            note: "Tools · lazygit".into(),
            cmd: "display-popup -E \"lazygit\"".into(),
        });
    }

    /// The reason for matching on the command: the log never says which table `[` came from.
    #[test]
    fn a_press_files_under_the_binding_whose_command_it_ran() {
        let b = parse_bindings(BINDINGS);
        assert_eq!(match_binding(&b, "[", "send-keys -X previous-prompt").map(|b| b.table.as_str()), Some("copy-mode-vi"));
        assert_eq!(match_binding(&b, "[", "previous-window").map(|b| b.table.as_str()), Some("prefix"));
        assert_eq!(match_binding(&b, "g", "display-popup -E \"old command\"").map(|b| b.key.as_str()), Some("g"));
        assert_eq!(match_binding(&b, "[", "something-else"), None);
        assert_eq!(match_binding(&b, "Z", "resize-pane -Z"), None);
    }
}
