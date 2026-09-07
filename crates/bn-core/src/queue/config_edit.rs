//! Writing the queue's own knobs back into `config.toml`.
//!
//! Line-oriented rather than a TOML round-trip, and for a reason worth stating once: `config.toml`
//! is a file someone writes by hand, and this one is full of comments explaining why each knob is
//! set the way it is. A serializer would give it back correct and stripped of every one of them —
//! so an editor that reformats the file is an editor nobody will let near it twice.
//!
//! The same choice `queue::window`'s editor makes, for the same reason. This is the `[queue.jobs]`
//! half: which jobs this machine calls expensive, and how urgent each one is.

use std::collections::BTreeMap;

/// The `[queue.jobs]` block, as a caller edits it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobClasses {
    /// Jobs this machine calls heavy. `None` means "say nothing", which leaves bn's built-in list
    /// in force — distinct from `Some(vec![])`, which says *nothing here is heavy*.
    pub heavy: Option<Vec<String>>,
    /// Per-job priority overrides. Sorted, so a rewrite of an unchanged config is a no-op diff.
    pub priority: BTreeMap<String, i64>,
}

impl JobClasses {
    /// Whether this says anything at all. An empty block is removed rather than written, so a
    /// machine that reverts to the defaults ends up with the file it started with.
    pub fn is_empty(&self) -> bool {
        self.heavy.is_none() && self.priority.is_empty()
    }
}

/// Replace `[queue.jobs]` in `content` with `classes`, leaving every other line untouched.
pub fn edit_job_classes(content: &str, classes: &JobClasses) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut lines = content.lines().peekable();
    while let Some(line) = lines.next() {
        if line.trim() != HEADER {
            out.push(line.to_string());
            continue;
        }
        // Skip the block: everything up to the next table header. A `[queue.jobs.x]` sub-table
        // would start with `[`, so this stops there too — which is correct, since this writer does
        // not produce one and anything that did is not ours to carry forward.
        while let Some(next) = lines.peek() {
            if next.trim_start().starts_with('[') {
                break;
            }
            lines.next();
        }
    }
    while out.last().is_some_and(|line| line.trim().is_empty()) {
        out.pop();
    }
    if !classes.is_empty() {
        out.push(String::new());
        out.push(HEADER.to_string());
        if let Some(heavy) = &classes.heavy {
            let list = heavy
                .iter()
                .map(|j| format!("\"{}\"", escape(j)))
                .collect::<Vec<_>>()
                .join(", ");
            out.push(format!("heavy = [{list}]"));
        }
        if !classes.priority.is_empty() {
            let pairs = classes
                .priority
                .iter()
                .map(|(job, p)| format!("\"{}\" = {p}", escape(job)))
                .collect::<Vec<_>>()
                .join(", ");
            // An inline table: a `[queue.jobs.priority]` sub-table would parse identically, but it
            // would sit *after* whatever follows `[queue.jobs]` in the file and this writer would
            // then have to reason about ordering. One line cannot.
            out.push(format!("priority = {{ {pairs} }}"));
        }
    }
    let mut text = out.join("\n");
    text.push('\n');
    text
}

/// Write `classes` into the config at `path`.
pub fn write_job_classes(path: &std::path::Path, classes: &JobClasses) -> std::io::Result<()> {
    let current = std::fs::read_to_string(path).unwrap_or_default();
    std::fs::write(path, edit_job_classes(&current, classes))
}

const HEADER: &str = "[queue.jobs]";

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(heavy: Option<&[&str]>, priority: &[(&str, i64)]) -> JobClasses {
        JobClasses {
            heavy: heavy.map(|h| h.iter().map(|s| s.to_string()).collect()),
            priority: priority.iter().map(|(j, p)| (j.to_string(), *p)).collect(),
        }
    }

    /// The property the whole line-oriented approach exists for: a hand-written config keeps its
    /// comments, its ordering and its unrelated tables.
    #[test]
    fn editing_preserves_the_rest_of_the_file() {
        let before = "# why this machine is the way it is\n\
                      [personal]\n\
                      notes_root = \"/n\"\n\
                      \n\
                      [queue]\n\
                      coding = 2  # deliberately low\n";
        let after = edit_job_classes(before, &classes(Some(&["build"]), &[]));
        assert!(after.contains("# why this machine is the way it is"));
        assert!(after.contains("coding = 2  # deliberately low"));
        assert!(after.contains("[queue.jobs]"));
        assert!(after.contains("heavy = [\"build\"]"));
    }

    /// A second edit replaces the block rather than appending a second one — two `[queue.jobs]`
    /// tables is not valid TOML, and the file would stop loading entirely.
    #[test]
    fn a_second_edit_replaces_rather_than_appends() {
        let once = edit_job_classes(
            "[personal]\nnotes_root = \"/n\"\n",
            &classes(Some(&["a"]), &[]),
        );
        let twice = edit_job_classes(&once, &classes(Some(&["b"]), &[]));
        assert_eq!(twice.matches("[queue.jobs]").count(), 1, "{twice}");
        assert!(twice.contains("heavy = [\"b\"]"));
        assert!(!twice.contains("\"a\""));
    }

    /// Reverting to the defaults removes the block, so the file ends up as it started rather than
    /// carrying an empty table that says nothing.
    #[test]
    fn an_empty_block_is_removed_not_written() {
        let with = edit_job_classes("[personal]\nx = 1\n", &classes(Some(&["a"]), &[]));
        let without = edit_job_classes(&with, &JobClasses::default());
        assert!(!without.contains("[queue.jobs]"), "{without}");
        assert!(without.contains("x = 1"));
    }

    /// "Nothing is heavy" is a real thing to say, and it is not the same as saying nothing.
    #[test]
    fn an_explicit_empty_heavy_list_survives() {
        let text = edit_job_classes("[personal]\nx = 1\n", &classes(Some(&[]), &[]));
        assert!(text.contains("heavy = []"), "{text}");
    }

    /// Priorities round-trip as an inline table, sorted — so rewriting an unchanged config is a
    /// no-op diff rather than a reshuffle.
    #[test]
    fn priorities_are_written_sorted() {
        let text = edit_job_classes("", &classes(None, &[("zeta", 1), ("alpha", -5)]));
        assert!(
            text.contains("priority = { \"alpha\" = -5, \"zeta\" = 1 }"),
            "{text}"
        );
    }

    /// And what it writes is what bn reads back — the round trip that makes the editor trustworthy.
    #[test]
    fn what_it_writes_parses_back_to_what_went_in() {
        let want = classes(Some(&["build", "verify"]), &[("build", 10)]);
        let text = edit_job_classes("[personal]\nnotes_root = \"/n\"\n", &want);
        let parsed: toml::Value = toml::from_str(&text).expect("valid TOML");
        let jobs = parsed
            .get("queue")
            .and_then(|q| q.get("jobs"))
            .expect("the block is there");
        assert_eq!(
            jobs.get("heavy")
                .and_then(|h| h.as_array())
                .map(|a| a.len()),
            Some(2)
        );
        assert_eq!(
            jobs.get("priority")
                .and_then(|p| p.get("build"))
                .and_then(|v| v.as_integer()),
            Some(10)
        );
    }
}
