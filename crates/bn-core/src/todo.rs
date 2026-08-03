//! The strong todo file — a per-repo `TODO.txt` in [todo.txt](http://todotxt.org) grammar,
//! extended with the attention-list vocabulary as `key:value` metadata. One task per line;
//! nothing is ever purged (done and cancelled lines stay), so the file is the complete record.
//!
//! The line grammar is 100% todo.txt-compatible, so any todo.txt editor plugin reads it:
//!
//! ```text
//! [x] [(A-C)] [completion-date] [creation-date] description [+project] [@context] [key:value…]
//! ```
//!
//! The `key:value` slots carry what the attention list ([`crate::attention`]) tracks as typed
//! event fields, so a repo todo folds into `bn doctor` alongside notes, cron, and attn items:
//!
//! | token         | attention concept              |
//! |---------------|--------------------------------|
//! | `(A)/(B)/(C)` | severity urgent / warn / info  |
//! | `id:t3`       | stable item id                 |
//! | `branch:x`    | scope tag (the file is per-repo) |
//! | `src:agent`   | source / provenance            |
//! | `key:…`       | dedup key for idempotent pulls |
//! | `snooze:DATE` | deferred / hidden until a date |
//! | `due:DATE`    | due date                       |
//! | `from:a3`     | promoted-from-attention trail  |
//! | `uid:9f2a…`   | immutable identity (see below) |
//! | `mod:<epoch>` | last-modified stamp (see below)|
//!
//! # Surviving a union merge (#995)
//!
//! The file is `merge=union`, so a git sync **concatenates** both sides rather than conflicting.
//! [`append_task`] is safe under that: two machines each appending land two distinct lines. But
//! every *mutation* ([`save`]) is a full rewrite, and two machines that each rewrite produce two
//! copies of **every** line. Two mechanisms keep that from corrupting the store, and they solve
//! genuinely different problems:
//!
//! - **`uid:`** — minted once at creation, never changed. Two lines sharing a `uid:` are the same
//!   task rewritten on two machines, so [`TodoFile::dedupe_by_uid`] collapses them, keeping the
//!   highest `mod:`.
//! - **`id:`** — the short `t<N>` handle you type. It is *positional*, so two machines working
//!   offline can independently mint `t6` for two **different** tasks. [`TodoFile::heal_ids`]
//!   renumbers the collision so both survive.
//!
//! Deduplicating on `id:` would be wrong: it cannot tell "one task written twice" from "two tasks
//! that collided on a handle", and collapsing the latter silently deletes a task. So [`load`]
//! runs `dedupe_by_uid` (identity) **before** `heal_ids` (handles) — the identity pass removes the
//! true duplicates, and only genuine collisions are left for renumbering.
//!
//! Lifecycle is inline and self-contained: **open** (no `x`), **in-progress** (`@wip` context),
//! **done** (`x <date>`), **cancelled** (`x <date> cancelled:1`, distinct from a real completion
//! the way `attn_remove` is distinct from `attn_ack`). Snoozed items stay in the file but fold
//! out of the open set until their date.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Meta key holding a task's immutable identity (see the module docs on union merges).
pub const UID_KEY: &str = "uid";
/// Meta key holding a task's last-modified epoch seconds.
pub const MOD_KEY: &str = "mod";

/// Mint an identity for a brand-new task: unique across machines and across tasks created in the
/// same second on one machine.
///
/// Nanoseconds plus a process-local counter cover the same-machine case (two `add`s in one second
/// still differ), and hashing the task text in makes a cross-machine collision require the same
/// text at the same nanosecond. This is identity, not a security token — the cost of a collision
/// is one wrongly-merged line, so a short opaque hex string is the right trade.
fn mint_uid(text: &str) -> String {
    use std::hash::{Hash, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    nanos.hash(&mut h);
    COUNTER.fetch_add(1, Ordering::Relaxed).hash(&mut h);
    text.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// One classified whitespace token in a task's description. Projects/contexts/meta are stored
/// without their sigil; `Word` is anything else and passes through verbatim on render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Word(String),
    /// `+project` — an epic/grouping tag.
    Project(String),
    /// `@context` — where/how the task happens (`@wip`, `@review`).
    Context(String),
    /// `key:value` — the extension slot that carries attention-list fields.
    Meta(String, String),
}

impl Token {
    /// Render back to its on-disk token, sigil restored.
    fn render(&self) -> String {
        match self {
            Token::Word(w) => w.clone(),
            Token::Project(p) => format!("+{p}"),
            Token::Context(c) => format!("@{c}"),
            Token::Meta(k, v) => format!("{k}:{v}"),
        }
    }

    /// Classify one whitespace-delimited token. `+x`/`@x` need a non-empty tail; a `key:value`
    /// needs an identifier key (`[A-Za-z][A-Za-z0-9_-]*`) and a value that does not start with
    /// `/` — the guard that keeps `https://…` a plain word rather than a bogus `https:` pair.
    /// The value itself may contain further colons, so `key:pr-queue:bn#51` round-trips whole.
    fn classify(tok: &str) -> Token {
        if let Some(p) = tok.strip_prefix('+') {
            if !p.is_empty() {
                return Token::Project(p.to_string());
            }
        }
        if let Some(c) = tok.strip_prefix('@') {
            if !c.is_empty() {
                return Token::Context(c.to_string());
            }
        }
        if let Some((k, v)) = split_meta(tok) {
            return Token::Meta(k, v);
        }
        Token::Word(tok.to_string())
    }
}

/// Split a `key:value` token on its first colon, or `None` if it isn't a valid meta pair.
fn split_meta(tok: &str) -> Option<(String, String)> {
    let idx = tok.find(':')?;
    let (key, val) = (&tok[..idx], &tok[idx + 1..]);
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return None,
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    if val.starts_with('/') {
        return None; // e.g. https://… — keep it a Word
    }
    Some((key.to_string(), val.to_string()))
}

/// The folded lifecycle state of a task on a given day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Open,
    /// Marked `@wip` — actively in progress. Counts as open.
    Wip,
    /// `snooze:DATE` still in the future — folds out of the open set until then.
    Snoozed,
    Done,
    /// Completed as cancelled (`x … cancelled:1`), not a real finish.
    Cancelled,
}

/// One task line, parsed structurally but faithful on round-trip.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Task {
    pub done: bool,
    /// Priority letter `A`–`Z` (we assign only A/B/C = urgent/warn/info, but read any).
    pub priority: Option<char>,
    /// Completion date — only meaningful when `done`.
    pub completion: Option<String>,
    pub created: Option<String>,
    /// The description as classified tokens, in original order.
    pub desc: Vec<Token>,
}

impl Task {
    /// Value of `key:` meta, if present.
    pub fn meta(&self, key: &str) -> Option<&str> {
        self.desc.iter().find_map(|t| match t {
            Token::Meta(k, v) if k == key => Some(v.as_str()),
            _ => None,
        })
    }

    /// Set (or insert) a `key:value` meta, preserving position when it already exists, and stamp
    /// `mod:` with the current epoch.
    ///
    /// Stamping here rather than at each call site is deliberate: `mod:` is what
    /// [`TodoFile::dedupe_by_uid`] uses to pick the winner after a union merge, so a mutation that
    /// forgot to stamp would lose to an older copy of itself. The clock honors `$BN_NOW`
    /// ([`crate::util::now_epoch_secs`]), so tests pin it the same way the rest of bn does.
    ///
    /// Use [`Task::set_meta_raw`] for writes that are *not* user edits — id healing on read, and
    /// the identity fields themselves.
    pub fn set_meta(&mut self, key: &str, val: &str) {
        self.set_meta_raw(key, val);
        // `mod:` and `uid:` are bookkeeping, not content — stamping on them would recurse.
        if key != MOD_KEY && key != UID_KEY {
            self.touch();
        }
    }

    /// Set a meta **without** stamping `mod:`.
    ///
    /// For writes that must not count as a modification: [`TodoFile::heal_ids`] runs on every
    /// [`load`], and if healing stamped, merely listing tasks would make a stale copy look newer
    /// than the real one.
    pub fn set_meta_raw(&mut self, key: &str, val: &str) {
        for t in &mut self.desc {
            if let Token::Meta(k, v) = t {
                if k == key {
                    *v = val.to_string();
                    return;
                }
            }
        }
        self.desc
            .push(Token::Meta(key.to_string(), val.to_string()));
    }

    /// Stamp `mod:` with the current epoch — "this line was just edited".
    ///
    /// The stamp is moved to the end of the line rather than updated in place, so it always trails
    /// the task's real content instead of wedging itself between whatever two keys happened to be
    /// edited first. Keeps rendered lines stable and readable across repeated edits.
    pub fn touch(&mut self) {
        self.desc
            .retain(|t| !matches!(t, Token::Meta(k, _) if k == MOD_KEY));
        self.desc.push(Token::Meta(
            MOD_KEY.to_string(),
            crate::util::now_epoch_secs().to_string(),
        ));
    }

    /// The task's `id:` (e.g. `t3`), if assigned. A *positional* handle: [`TodoFile::heal_ids`]
    /// may renumber it after a merge, so never treat it as identity — that's [`Task::uid`].
    pub fn id(&self) -> Option<&str> {
        self.meta("id")
    }

    /// The task's immutable identity, minted at creation and never rewritten. `None` for lines
    /// authored before `uid:` existed (or by hand), which [`TodoFile::dedupe_by_uid`] then leaves
    /// alone rather than guessing.
    pub fn uid(&self) -> Option<&str> {
        self.meta(UID_KEY)
    }

    /// Last-modified epoch seconds from `mod:`. `0` when absent, so an unstamped legacy line
    /// always loses to a stamped one.
    pub fn modified(&self) -> i64 {
        self.meta(MOD_KEY)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
    }

    /// The `+project` tags.
    pub fn projects(&self) -> Vec<&str> {
        self.desc
            .iter()
            .filter_map(|t| match t {
                Token::Project(p) => Some(p.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The `@context` tags.
    pub fn contexts(&self) -> Vec<&str> {
        self.desc
            .iter()
            .filter_map(|t| match t {
                Token::Context(c) => Some(c.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Human-facing description: words, projects, and contexts joined — meta stripped.
    pub fn summary(&self) -> String {
        self.desc
            .iter()
            .filter(|t| !matches!(t, Token::Meta(..)))
            .map(Token::render)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Fold the lifecycle state as of `today` (`YYYY-MM-DD`). Snooze compares lexically, which is
    /// correct for zero-padded ISO dates, matching [`crate::attention`].
    pub fn status(&self, today: &str) -> Status {
        if self.done {
            return if self.meta("cancelled").is_some() {
                Status::Cancelled
            } else {
                Status::Done
            };
        }
        if let Some(s) = self.meta("snooze") {
            // Only a well-formed date snoozes. A malformed value (a hand-edit like `snooze:soon`,
            // or a merge artifact) sorts lexically greater than any `YYYY-MM-DD`, which would fold
            // the task out of the open set forever — so ignore it and keep the task visible.
            if is_date(s) && s > today {
                return Status::Snoozed;
            }
        }
        if self.contexts().contains(&"wip") {
            Status::Wip
        } else {
            Status::Open
        }
    }

    /// Open work as of `today` — not done, not snoozed into the future. `@wip` counts as open.
    pub fn is_open(&self, today: &str) -> bool {
        matches!(self.status(today), Status::Open | Status::Wip)
    }

    /// Mark the task done on `today`. Priority is kept (useful for metrics), unlike some todo.txt
    /// tools that drop it.
    pub fn mark_done(&mut self, today: &str) {
        self.done = true;
        self.completion = Some(today.to_string());
        self.touch();
    }

    /// Mark done *as cancelled* — folds out of the open set like [`Task::mark_done`], but records
    /// `cancelled:1` (+ optional `reason:`) so it's distinguishable from a real completion.
    pub fn cancel(&mut self, today: &str, reason: Option<&str>) {
        self.mark_done(today);
        self.set_meta("cancelled", "1");
        if let Some(r) = reason {
            self.set_meta("reason", r);
        }
    }

    /// Mark in progress by adding the `@wip` context (idempotent — no duplicate).
    pub fn mark_wip(&mut self) {
        if !self.contexts().contains(&"wip") {
            self.desc.push(Token::Context("wip".to_string()));
            self.touch();
        }
    }

    /// A stable JSON view for `--json` output — the folded status plus the fields callers care
    /// about. Keeps the CLI free of `Token`-shaped internals.
    pub fn to_json(&self, today: &str) -> serde_json::Value {
        serde_json::json!({
            "id": self.id(),
            "status": format!("{:?}", self.status(today)).to_lowercase(),
            "priority": self.priority.map(|c| c.to_string()),
            "summary": self.summary(),
            "created": self.created,
            "completion": self.completion,
            "due": self.meta("due"),
            "snooze": self.meta("snooze"),
            "branch": self.meta("branch"),
            "src": self.meta("src"),
            "projects": self.projects(),
            "contexts": self.contexts(),
        })
    }

    /// Sort key so open work reads well: priority first (`A` < `B` < `C`, then unprioritized),
    /// then earliest due date (undated last), then id. Returns owned strings so it composes with
    /// `sort_by_key` on borrowed tasks.
    fn open_sort_key(&self) -> (u8, String, u64) {
        let pri = self.priority.map(|c| c as u8).unwrap_or(u8::MAX);
        let due = self.meta("due").unwrap_or("~").to_string(); // '~' > any digit ⇒ undated last
        let idn = self
            .id()
            .and_then(|id| id.strip_prefix('t'))
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(u64::MAX);
        (pri, due, idn)
    }

    /// A fresh open todo from free `text` (which may inline `+project`/`@context`), stamped with
    /// `id`, `created` date, `branch:` scope, and `src:` provenance in that trailing order.
    pub fn new_todo(
        id: &str,
        text: &str,
        priority: Option<char>,
        branch: &str,
        src: &str,
        today: &str,
    ) -> Task {
        let mut task = Task {
            done: false,
            priority,
            completion: None,
            created: Some(today.to_string()),
            desc: text.split_whitespace().map(Token::classify).collect(),
        };
        task.set_meta_raw("id", id);
        task.set_meta_raw("branch", branch);
        task.set_meta_raw("src", src);
        // Identity first, then the stamp — every task is born dedupable (#995).
        task.set_meta_raw(UID_KEY, &mint_uid(text));
        task.touch();
        task
    }

    /// Render the task back to a single todo.txt line.
    pub fn render(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.done {
            parts.push("x".to_string());
        }
        if let Some(p) = self.priority {
            parts.push(format!("({p})"));
        }
        // A creation date can't be shown without a completion date on a done task — the parser
        // would read the lone date as the completion. So emit the date pair only when it round-
        // trips: both for a done task, creation only for an open one.
        if self.done {
            if let Some(c) = &self.completion {
                parts.push(c.clone());
                if let Some(cr) = &self.created {
                    parts.push(cr.clone());
                }
            }
        } else if let Some(cr) = &self.created {
            parts.push(cr.clone());
        }
        parts.extend(self.desc.iter().map(Token::render));
        parts.join(" ")
    }
}

/// A date-shaped `YYYY-MM-DD` token (structure only; not calendar-validated).
fn is_date(tok: &str) -> bool {
    let b = tok.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| matches!(i, 4 | 7) || c.is_ascii_digit())
}

/// A `(A)`–`(Z)` priority token.
fn priority_of(tok: &str) -> Option<char> {
    let b = tok.as_bytes();
    if b.len() == 3 && b[0] == b'(' && b[2] == b')' && b[1].is_ascii_uppercase() {
        Some(b[1] as char)
    } else {
        None
    }
}

/// Parse one non-empty line into a [`Task`]. Never fails: unrecognized leading tokens simply
/// fall through into the description.
fn parse_task(line: &str) -> Task {
    let toks: Vec<&str> = line.split_whitespace().collect();
    let mut task = Task::default();
    let mut i = 0;

    if toks.first() == Some(&"x") {
        task.done = true;
        i = 1;
    }
    if let Some(p) = toks.get(i).and_then(|t| priority_of(t)) {
        task.priority = Some(p);
        i += 1;
    }
    // Leading dates: up to two for a done task (completion, creation), one for an open task.
    let max_dates = if task.done { 2 } else { 1 };
    let mut dates: Vec<&str> = Vec::new();
    while dates.len() < max_dates {
        match toks.get(i) {
            Some(t) if is_date(t) => {
                dates.push(t);
                i += 1;
            }
            _ => break,
        }
    }
    if task.done {
        match dates.as_slice() {
            [comp, created] => {
                task.completion = Some(comp.to_string());
                task.created = Some(created.to_string());
            }
            [comp] => task.completion = Some(comp.to_string()),
            _ => {}
        }
    } else if let Some(d) = dates.first() {
        task.created = Some(d.to_string());
    }

    task.desc = toks[i..].iter().map(|t| Token::classify(t)).collect();
    task
}

/// One physical line of the file, preserving blanks so hand-formatting survives a rewrite.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Line {
    Task(Task),
    /// A blank/whitespace-only line, kept for round-trip fidelity.
    Blank,
}

/// A parsed `TODO.txt`: an ordered list of task and blank lines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TodoFile {
    lines: Vec<Line>,
}

impl TodoFile {
    /// Parse a `TODO.txt`'s contents. Blank lines are preserved; every other line is a task.
    pub fn parse(input: &str) -> TodoFile {
        let lines = input
            .lines()
            .map(|l| {
                if l.trim().is_empty() {
                    Line::Blank
                } else {
                    Line::Task(parse_task(l))
                }
            })
            .collect();
        TodoFile { lines }
    }

    /// Render back to file text with a trailing newline (empty file ⇒ empty string).
    pub fn render(&self) -> String {
        if self.lines.is_empty() {
            return String::new();
        }
        let body = self
            .lines
            .iter()
            .map(|l| match l {
                Line::Task(t) => t.render(),
                Line::Blank => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("{body}\n")
    }

    /// All tasks in file order.
    pub fn tasks(&self) -> impl Iterator<Item = &Task> {
        self.lines.iter().filter_map(|l| match l {
            Line::Task(t) => Some(t),
            Line::Blank => None,
        })
    }

    /// Mutably borrow a task by its `id:`.
    pub fn task_mut(&mut self, id: &str) -> Option<&mut Task> {
        self.lines.iter_mut().find_map(|l| match l {
            Line::Task(t) if t.id() == Some(id) => Some(t),
            _ => None,
        })
    }

    /// The next `t<N>` id: one past the highest ever assigned, so ids of done/cancelled tasks are
    /// never reused (matching the note-section id discipline in [`crate::id`]).
    pub fn next_id(&self) -> String {
        let max = self
            .tasks()
            .filter_map(|t| t.id())
            .filter_map(|id| id.strip_prefix('t').and_then(|n| n.parse::<u64>().ok()))
            .max()
            .unwrap_or(0);
        format!("t{}", max + 1)
    }

    /// Append a new task built from free `text` (which may inline `+project`/`@context`), stamped
    /// with a fresh id, `created` date, `branch:` scope, and `src:` provenance. Returns the id.
    pub fn add(
        &mut self,
        text: &str,
        priority: Option<char>,
        branch: &str,
        src: &str,
        today: &str,
    ) -> String {
        let id = self.next_id();
        let task = Task::new_todo(&id, text, priority, branch, src, today);
        self.lines.push(Line::Task(task));
        id
    }

    /// Append an already-built task as the file's last line. The CLI `add` path builds the task
    /// itself (to stamp `due:` etc.) and normally persists it via the concurrency-safe
    /// [`append_task`]; it falls back to this + a full [`save`] only when the loaded file had to be
    /// healed, so the corrected id numbering is written durably rather than re-derived on next read.
    pub fn push_task(&mut self, task: Task) {
        self.lines.push(Line::Task(task));
    }

    /// Make every task's `id:` unique and present. A union-merge of the shared file (see
    /// `.gitattributes`) can concatenate two branches that each minted `t6`, and hand-added lines
    /// may carry no `id:` at all. In file order, the first valid unique `t<N>` is kept; any
    /// duplicate, malformed, or missing id is reassigned to a fresh `t<max+1>`. Deterministic, so
    /// repeated read-only `list`s show stable ids; a later mutation persists the healed numbering.
    pub fn heal_ids(&mut self) {
        use std::collections::HashSet;
        let parse = |id: &str| id.strip_prefix('t').and_then(|n| n.parse::<u64>().ok());
        let mut max = self
            .tasks()
            .filter_map(|t| t.id().and_then(parse))
            .max()
            .unwrap_or(0);
        let mut seen: HashSet<String> = HashSet::new();
        for line in &mut self.lines {
            let Line::Task(t) = line else { continue };
            let keep = t
                .id()
                .filter(|id| parse(id).is_some() && !seen.contains(*id))
                .map(str::to_string);
            match keep {
                Some(id) => {
                    seen.insert(id);
                }
                None => {
                    max += 1;
                    let nid = format!("t{max}");
                    // Raw: healing runs on every `load`, and stamping `mod:` here would make a
                    // merely-listed stale copy outrank the real one in `dedupe_by_uid`.
                    t.set_meta_raw("id", &nid);
                    seen.insert(nid);
                }
            }
        }
    }

    /// Collapse lines that a union merge duplicated, keeping each task's newest version.
    ///
    /// Two lines are the same task when they share a [`Task::uid`] — the identity minted once at
    /// creation. The survivor is the one with the highest [`Task::modified`]; ties go to the last
    /// in file order, so the result is deterministic regardless of which side of the merge landed
    /// first. Each survivor stays at its own position, and non-task lines are untouched.
    ///
    /// Tasks with no `uid:` are left untouched — a hand-added or pre-#995 line has no identity to
    /// match on, and guessing would risk deleting real work.
    ///
    /// **Not keyed on `id:`.** Two machines working offline can each mint `t6` for two different
    /// tasks; collapsing those would silently delete one. That case is [`TodoFile::heal_ids`]'s,
    /// and it runs after this pass. Returns the number of lines removed.
    pub fn dedupe_by_uid(&mut self) -> usize {
        use std::collections::HashMap;

        // uid → (index of the winning line so far, its `mod:`)
        let mut winner: HashMap<String, (usize, i64)> = HashMap::new();
        for (idx, line) in self.lines.iter().enumerate() {
            let Line::Task(t) = line else { continue };
            let Some(uid) = t.uid().map(str::to_string) else {
                continue;
            };
            let m = t.modified();
            match winner.get(&uid) {
                // `>=` so a tie resolves to the later line.
                Some(&(_, best)) if m >= best => {
                    winner.insert(uid, (idx, m));
                }
                Some(_) => {}
                None => {
                    winner.insert(uid, (idx, m));
                }
            }
        }

        let keep: std::collections::HashSet<usize> = winner.values().map(|&(idx, _)| idx).collect();
        let before = self.lines.len();
        let mut idx = 0;
        self.lines.retain(|line| {
            let this = idx;
            idx += 1;
            match line {
                Line::Task(t) if t.uid().is_some() => keep.contains(&this),
                _ => true,
            }
        });
        before - self.lines.len()
    }

    /// Open tasks as of `today`, optionally filtered to one `branch`, in display order (see
    /// [`Task::open_sort_key`]).
    pub fn open_sorted(&self, today: &str, branch: Option<&str>) -> Vec<&Task> {
        let mut v: Vec<&Task> = self
            .tasks()
            .filter(|t| t.is_open(today))
            .filter(|t| branch.is_none_or(|b| t.meta("branch") == Some(b)))
            .collect();
        v.sort_by_key(|t| t.open_sort_key());
        v
    }

    /// Mark task `id` done on `today`. `false` if no such id.
    pub fn mark_done(&mut self, id: &str, today: &str) -> bool {
        self.task_mut(id).map(|t| t.mark_done(today)).is_some()
    }

    /// Mark task `id` in progress (`@wip`). `false` if no such id.
    pub fn mark_wip(&mut self, id: &str) -> bool {
        self.task_mut(id).map(|t| t.mark_wip()).is_some()
    }

    /// Snooze task `id` until `until`. `false` if no such id.
    pub fn snooze(&mut self, id: &str, until: &str) -> bool {
        self.task_mut(id)
            .map(|t| t.set_meta("snooze", until))
            .is_some()
    }

    /// Cancel task `id` on `today` with an optional reason. `false` if no such id.
    pub fn cancel(&mut self, id: &str, today: &str, reason: Option<&str>) -> bool {
        self.task_mut(id).map(|t| t.cancel(today, reason)).is_some()
    }

    /// Batch-ingest script-provided todos. `key`s are namespaced by source (`<src>:<key>`) so
    /// independent scripts can reuse natural keys without colliding; a re-ingest with a key that
    /// an *open* todo already carries is skipped (idempotent polling). With `replace`, open keyed
    /// todos from the batch's source(s) that are absent from this batch are marked done — letting
    /// a polling script emit its current set and have stale entries close themselves. Mirrors
    /// [`crate::attention::ingest`]. Validates priorities/dates up front, erroring the whole batch
    /// on a bad row (parity with attn), so a typo never half-applies.
    pub fn ingest(
        &mut self,
        items: &[IngestTodo],
        default_src: &str,
        default_branch: &str,
        replace: bool,
        today: &str,
    ) -> Result<IngestOutcome, String> {
        use std::collections::{HashMap, HashSet};
        let non_empty = |s: &str| -> Option<String> {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_string())
        };
        let default_src = non_empty(default_src).unwrap_or_else(|| "script".to_string());

        // Validate the whole batch before touching `self` — a bad priority/date must error the
        // batch atomically (per the doc contract), never half-apply after earlier rows were pushed.
        for item in items {
            let Some(text) = non_empty(&item.text) else {
                continue;
            };
            ingest_priority(item.priority.as_deref(), item.severity.as_deref())?;
            for (label, val) in [("due", &item.due), ("snooze", &item.snooze)] {
                if let Some(d) = val.as_deref().and_then(non_empty) {
                    if crate::util::epoch_day(&d).is_none() {
                        return Err(format!(
                            "invalid {label} '{d}' for '{text}' (want YYYY-MM-DD)"
                        ));
                    }
                }
            }
        }

        let mut out = IngestOutcome::default();
        let mut current_keys: HashMap<String, HashSet<String>> = HashMap::new();
        let mut affected: HashSet<String> = HashSet::new();
        if replace {
            affected.insert(default_src.clone());
        }

        for item in items {
            let Some(text) = non_empty(&item.text) else {
                continue;
            };
            let src = item
                .src
                .as_deref()
                .and_then(non_empty)
                .unwrap_or_else(|| default_src.clone());
            let branch = item
                .branch
                .as_deref()
                .and_then(non_empty)
                .unwrap_or_else(|| default_branch.to_string());
            let priority = ingest_priority(item.priority.as_deref(), item.severity.as_deref())?;
            for (label, val) in [("due", &item.due), ("snooze", &item.snooze)] {
                if let Some(d) = val.as_deref().and_then(non_empty) {
                    if crate::util::epoch_day(&d).is_none() {
                        return Err(format!(
                            "invalid {label} '{d}' for '{text}' (want YYYY-MM-DD)"
                        ));
                    }
                }
            }

            affected.insert(src.clone());
            let nkey = item
                .key
                .as_deref()
                .and_then(non_empty)
                .map(|k| format!("{src}:{k}"));
            if let Some(k) = &nkey {
                current_keys
                    .entry(src.clone())
                    .or_default()
                    .insert(k.clone());
                if self
                    .tasks()
                    .any(|t| t.is_open(today) && t.meta("key") == Some(k.as_str()))
                {
                    out.skipped += 1;
                    continue;
                }
            }

            let id = self.next_id();
            let mut task = Task::new_todo(&id, &text, priority, &branch, &src, today);
            if let Some(d) = item.due.as_deref().and_then(non_empty) {
                task.set_meta("due", &d);
            }
            if let Some(s) = item.snooze.as_deref().and_then(non_empty) {
                task.set_meta("snooze", &s);
            }
            if let Some(k) = &nkey {
                task.set_meta("key", k);
            }
            self.lines.push(Line::Task(task));
            out.added += 1;
            out.ids.push(id);
        }

        if replace {
            let stale: Vec<String> = self
                .tasks()
                .filter(|t| t.is_open(today))
                .filter_map(|t| {
                    let src = t.meta("src")?;
                    if !affected.contains(src) {
                        return None;
                    }
                    let key = t.meta("key")?;
                    if !key.starts_with(&format!("{src}:")) {
                        return None;
                    }
                    let kept = current_keys.get(src).is_some_and(|ks| ks.contains(key));
                    if kept {
                        None
                    } else {
                        t.id().map(str::to_string)
                    }
                })
                .collect();
            for id in stale {
                if self.mark_done(&id, today) {
                    out.resolved += 1;
                }
            }
        }

        Ok(out)
    }
}

/// Load the todo file at `path`, or an empty file when it doesn't exist yet (first `add` creates
/// it). Any I/O error other than "not found" propagates.
///
/// Two repair passes run on read, and **the order matters** (#995):
///
/// 1. [`TodoFile::dedupe_by_uid`] collapses lines a union merge duplicated, keeping each task's
///    newest version.
/// 2. [`TodoFile::heal_ids`] renumbers the `t<N>` handles that remain colliding, so the loaded
///    model is uniquely addressable.
///
/// Reversed, healing would hand one copy of a duplicated task a fresh id, after which nothing
/// could tell it apart from a genuinely distinct task — the duplicate would be permanent.
pub fn load(path: &Path) -> std::io::Result<TodoFile> {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let mut f = TodoFile::parse(&s);
            f.dedupe_by_uid();
            f.heal_ids();
            Ok(f)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TodoFile::default()),
        Err(e) => Err(e),
    }
}

/// Append one already-built task as a new line, without rewriting the existing file. Concurrent
/// `add`s then concatenate (both survive) instead of one clobbering the other via a full rewrite;
/// any duplicate id they mint is healed on the next [`load`]. Creates the file if absent and
/// inserts a separating newline if the current tail lacks one.
pub fn append_task(path: &Path, task: &Task) -> std::io::Result<()> {
    use std::io::Write;
    let existing = std::fs::read(path).unwrap_or_default();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    if !existing.is_empty() && existing.last() != Some(&b'\n') {
        f.write_all(b"\n")?;
    }
    writeln!(f, "{}", task.render())
}

/// One script-provided todo for [`TodoFile::ingest`]. Unknown fields are ignored, so a JSONL
/// stream shared with `attn ingest` just drops the fields todos don't use. `title` aliases
/// `text`; `severity` is an alternate source for `priority` (urgent→A, warn→B, info→C).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct IngestTodo {
    #[serde(alias = "title")]
    pub text: String,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub due: Option<String>,
    #[serde(default)]
    pub snooze: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub src: Option<String>,
}

/// Summary of an ingest batch.
#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
pub struct IngestOutcome {
    pub added: usize,
    pub skipped: usize,
    pub resolved: usize,
    pub ids: Vec<String>,
}

/// Parse ingest input: a JSON array, a single JSON object, or JSONL (one object per line).
/// Malformed JSONL lines are ignored, matching `attn ingest`'s tolerant shape.
pub fn parse_ingest_todos(input: &str) -> Vec<IngestTodo> {
    let trimmed = input.trim_start();
    if trimmed.starts_with('[') {
        if let Ok(items) = serde_json::from_str::<Vec<IngestTodo>>(trimmed) {
            return items;
        }
    }
    if trimmed.starts_with('{') {
        if let Ok(item) = serde_json::from_str::<IngestTodo>(trimmed) {
            return vec![item];
        }
    }
    input
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str::<IngestTodo>(l).ok())
        .collect()
}

/// Resolve a priority from either an explicit `A`/`B`/`C` (case-insensitive) or a severity word.
fn ingest_priority(priority: Option<&str>, severity: Option<&str>) -> Result<Option<char>, String> {
    if let Some(p) = priority.map(str::trim).filter(|s| !s.is_empty()) {
        let Some(c) = p.to_ascii_uppercase().chars().next() else {
            return Err("priority cannot be empty".to_string());
        };
        if p.len() == 1 && ('A'..='C').contains(&c) {
            return Ok(Some(c));
        }
        return Err(format!("invalid priority '{p}' (want A | B | C)"));
    }
    if let Some(s) = severity.map(str::trim).filter(|s| !s.is_empty()) {
        return match s.to_ascii_lowercase().as_str() {
            "urgent" => Ok(Some('A')),
            "warn" => Ok(Some('B')),
            "info" => Ok(Some('C')),
            _ => Err(format!(
                "invalid severity '{s}' (want urgent | warn | info)"
            )),
        };
    }
    Ok(None)
}

/// Write the todo file back to `path` atomically: render to a sibling temp file, then rename
/// over `path`. A crash or a racing writer can never observe a half-written `TODO.txt` — the
/// rename either fully lands or doesn't. It does not by itself prevent a concurrent whole-file
/// rewrite from clobbering an append; the CLI serializes each read-modify-write under [`lock`].
pub fn save(path: &Path, file: &TodoFile) -> std::io::Result<()> {
    let rendered = file.render();
    let tmp = match path.file_name() {
        Some(name) => path.with_file_name(format!(".{}.tmp", name.to_string_lossy())),
        None => path.with_extension("tmp"),
    };
    std::fs::write(&tmp, rendered)?;
    std::fs::rename(&tmp, path)
}

/// Ensure `<notes_root>/.gitattributes` marks the task store `merge=union`, so a sync
/// concatenates both sides instead of raising a conflict on a file two machines both append to.
///
/// Idempotent, and additive: an existing `.gitattributes` keeps its content (the notes root
/// already carries `**/events.jsonl merge=union`, the same treatment for the same reason). Silent
/// on I/O failure — a missing merge driver degrades to ordinary conflicts, which is annoying but
/// not lossy, and is never worth failing a task write over.
///
/// Union merge is only safe *because* [`load`] repairs the concatenation; see the module docs.
pub fn ensure_union_merge(notes_root: &Path) -> std::io::Result<()> {
    const LINE: &str = "tasks.txt merge=union";
    let ga = notes_root.join(".gitattributes");
    let existing = std::fs::read_to_string(&ga).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == LINE) {
        return Ok(());
    }
    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(LINE);
    content.push('\n');
    std::fs::write(&ga, content)
}

/// An advisory exclusive lock over a todo file's read-modify-write, released on drop. Serializes
/// `add` (an append) against mutate/ingest (a `save` rewrite + rename) so a rename can't clobber a
/// concurrent append and lose a task. Held for the whole load→modify→write span, not just the
/// write, since the race is the interleave across that span.
pub struct TodoLock(Option<std::fs::File>);

impl Drop for TodoLock {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            // Advisory lock releases on close anyway; an explicit unlock error is safe to drop.
            let _ = f.unlock();
        }
    }
}

/// Acquire the [`TodoLock`] for `path`, blocking until it's free. Best-effort: if the lock file
/// can't be created or locked (an odd filesystem, no state dir) it returns an inert guard and the
/// caller proceeds unlocked — never worse than the pre-lock behavior. The lock lives in the state
/// dir (not the repo, so no artifact is committed and it survives `save`'s rename), keyed by the
/// todo file's canonical path so every process touching the same `TODO.txt` contends on one lock.
pub fn lock(path: &Path) -> TodoLock {
    TodoLock(acquire_lock(path))
}

fn acquire_lock(path: &Path) -> Option<std::fs::File> {
    let dir = crate::resolver::state_dir().join("locks");
    std::fs::create_dir_all(&dir).ok()?;
    let file = std::fs::File::create(dir.join(format!("todo-{}.lock", lock_key(path)))).ok()?;
    file.lock().ok()?;
    Some(file)
}

/// A stable, filesystem-safe key for `path`: a hash of its canonicalized location, so a symlinked
/// and a literal spelling of the same file share one lock.
fn lock_key(path: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let canon = path
        .parent()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .map(|p| p.join(path.file_name().unwrap_or_default()))
        .unwrap_or_else(|| path.to_path_buf());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    canon.hash(&mut h);
    format!("{:016x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_mixed_file_verbatim() {
        let src = "\
(A) 2026-07-03 Fix flaky test @wip +test due:2026-07-10 id:t3 branch:stl src:agent
2026-07-02 Reply to thread snooze:2026-07-15 key:pr-queue:bn#51 branch:main src:cron
x 2026-07-03 2026-07-03 Design the format id:t1 branch:stl
x 2026-07-04 2026-07-02 Drop JSONL idea cancelled:1 reason:PR-conflicts id:t0 branch:main
";
        assert_eq!(TodoFile::parse(src).render(), src);
    }

    #[test]
    fn preserves_blank_lines() {
        let src = "(A) first id:t1\n\n2026-07-01 second id:t2\n";
        assert_eq!(TodoFile::parse(src).render(), src);
    }

    #[test]
    fn urls_stay_words_but_multicolon_values_are_meta() {
        let t = parse_task("Reply on https://mail.example/x key:pr-queue:bn#51 due:2026-07-10");
        assert!(t
            .desc
            .contains(&Token::Word("https://mail.example/x".to_string())));
        assert_eq!(t.meta("key"), Some("pr-queue:bn#51"));
        assert_eq!(t.meta("due"), Some("2026-07-10"));
    }

    #[test]
    fn parses_priority_and_leading_dates() {
        let open = parse_task("(B) 2026-07-01 do a thing +proj");
        assert_eq!(open.priority, Some('B'));
        assert_eq!(open.created.as_deref(), Some("2026-07-01"));
        assert!(!open.done);
        assert_eq!(open.projects(), vec!["proj"]);

        let done = parse_task("x 2026-07-05 2026-07-01 finished id:t9");
        assert!(done.done);
        assert_eq!(done.completion.as_deref(), Some("2026-07-05"));
        assert_eq!(done.created.as_deref(), Some("2026-07-01"));
        assert_eq!(done.id(), Some("t9"));
    }

    #[test]
    fn open_task_takes_only_one_leading_date() {
        // A stray second date on an open task is description, not a completion date.
        let t = parse_task("2026-07-01 2026-07-02 two dates");
        assert_eq!(t.created.as_deref(), Some("2026-07-01"));
        assert!(t.desc.contains(&Token::Word("2026-07-02".to_string())));
    }

    #[test]
    fn folds_status_across_lifecycle() {
        let today = "2026-07-10";
        assert_eq!(parse_task("plain thing").status(today), Status::Open);
        assert_eq!(parse_task("busy @wip").status(today), Status::Wip);
        assert_eq!(
            parse_task("later snooze:2026-07-15").status(today),
            Status::Snoozed
        );
        assert_eq!(
            parse_task("later snooze:2026-07-05").status(today),
            Status::Open,
            "a lapsed snooze reopens"
        );
        assert_eq!(
            parse_task("x 2026-07-09 did it").status(today),
            Status::Done
        );
        assert_eq!(
            parse_task("x 2026-07-09 nope cancelled:1").status(today),
            Status::Cancelled
        );
    }

    #[test]
    fn is_open_excludes_snoozed_and_done() {
        let today = "2026-07-10";
        assert!(parse_task("open one").is_open(today));
        assert!(parse_task("wip one @wip").is_open(today));
        assert!(!parse_task("snoozed snooze:2026-07-20").is_open(today));
        assert!(!parse_task("x 2026-07-01 done").is_open(today));
        // A malformed snooze value must not hide the task forever (letters sort > any ISO date).
        assert!(parse_task("bad snooze snooze:soon").is_open(today));
        assert_eq!(
            parse_task("bad snooze snooze:soon").status(today),
            Status::Open
        );
    }

    #[test]
    fn set_meta_updates_in_place_then_appends() {
        let mut t = parse_task("thing snooze:2026-07-01 id:t1");
        t.set_meta("snooze", "2026-08-01"); // update existing, keep position
        assert_eq!(t.meta("snooze"), Some("2026-08-01"));
        t.set_meta("due", "2026-09-01"); // new key appends
        assert_eq!(t.meta("due"), Some("2026-09-01"));
        // id must be untouched and still last-but-one
        assert_eq!(t.id(), Some("t1"));
        // A `mod:` stamp is appended after the edited keys (#995), so match the prefix only.
        assert!(t
            .render()
            .starts_with("thing snooze:2026-08-01 id:t1 due:2026-09-01"));
        assert!(t.modified() > 0, "an edit stamps mod:");
    }

    #[test]
    fn mark_done_and_cancel_round_trip() {
        let mut t = parse_task("2026-07-01 build it id:t1 branch:main");
        t.mark_done("2026-07-05");
        let done = t.render();
        assert!(done.starts_with("x 2026-07-05 2026-07-01 build it"));
        assert_eq!(parse_task(&done).status("2026-07-06"), Status::Done);

        let mut c = parse_task("2026-07-01 bad idea id:t2");
        c.cancel("2026-07-05", Some("wrong-approach"));
        assert_eq!(c.meta("cancelled"), Some("1"));
        assert_eq!(c.meta("reason"), Some("wrong-approach"));
        assert_eq!(
            parse_task(&c.render()).status("2026-07-06"),
            Status::Cancelled
        );
    }

    #[test]
    fn next_id_tracks_max_never_reuses() {
        let f = TodoFile::parse("x 2026-07-01 a id:t1\nb id:t3\n");
        assert_eq!(f.next_id(), "t4");
        assert_eq!(TodoFile::default().next_id(), "t1");
    }

    #[test]
    fn add_stamps_id_branch_src_and_created() {
        let mut f = TodoFile::parse("(A) existing id:t1 branch:main\n");
        let id = f.add(
            "write +docs @wip the spec",
            Some('B'),
            "stl",
            "manual",
            "2026-07-03",
        );
        assert_eq!(id, "t2");
        let t = f.task_mut("t2").unwrap();
        assert_eq!(t.priority, Some('B'));
        assert_eq!(t.created.as_deref(), Some("2026-07-03"));
        assert_eq!(t.meta("branch"), Some("stl"));
        assert_eq!(t.meta("src"), Some("manual"));
        assert_eq!(t.projects(), vec!["docs"]);
        assert_eq!(t.contexts(), vec!["wip"]);
        assert!(t.is_open("2026-07-03"));
    }

    #[test]
    fn empty_file_renders_empty() {
        assert_eq!(TodoFile::default().render(), "");
        assert_eq!(TodoFile::parse("").render(), "");
    }

    #[test]
    fn open_sorted_orders_by_priority_then_due_then_id() {
        let f = TodoFile::parse(
            "\
(B) later id:t1 branch:x due:2026-08-01
(A) urgent id:t2 branch:x
no-priority id:t3 branch:x due:2026-07-05
(A) earlier-due id:t4 branch:x due:2026-07-10
x 2026-07-01 done id:t5 branch:x
other-branch id:t6 branch:y
",
        );
        let ids: Vec<&str> = f
            .open_sorted("2026-07-03", Some("x"))
            .iter()
            .map(|t| t.id().unwrap())
            .collect();
        // Within priority A, the dated item leads the undated one; then B, then unprioritized.
        // done(t5) and branch y(t6) are excluded.
        assert_eq!(ids, vec!["t4", "t2", "t1", "t3"]);
    }

    #[test]
    fn open_sorted_all_branches_when_unfiltered() {
        let f = TodoFile::parse("a id:t1 branch:x\nb id:t2 branch:y\n");
        assert_eq!(f.open_sorted("2026-07-03", None).len(), 2);
    }

    #[test]
    fn file_ops_find_by_id_and_report_missing() {
        let mut f = TodoFile::parse("a thing id:t1 branch:x\n");
        assert!(f.mark_wip("t1"));
        assert!(f.task_mut("t1").unwrap().contexts().contains(&"wip"));
        assert!(f.snooze("t1", "2026-09-01"));
        assert_eq!(f.task_mut("t1").unwrap().meta("snooze"), Some("2026-09-01"));
        assert!(f.mark_done("t1", "2026-07-05"));
        assert!(f.task_mut("t1").unwrap().done);
        // unknown ids are reported, not panics
        assert!(!f.mark_done("t9", "2026-07-05"));
        assert!(!f.cancel("t9", "2026-07-05", None));
    }

    #[test]
    fn load_missing_file_is_empty_then_save_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TODO.txt");
        assert_eq!(load(&path).unwrap(), TodoFile::default());

        let mut f = TodoFile::default();
        f.add("ship it +rel", Some('A'), "main", "manual", "2026-07-03");
        save(&path, &f).unwrap();
        let reloaded = load(&path).unwrap();
        assert_eq!(reloaded, f);
        assert_eq!(reloaded.open_sorted("2026-07-03", Some("main")).len(), 1);
    }

    #[test]
    fn heal_ids_renumbers_dups_and_fills_missing() {
        // Simulates a union-merge: two branches each minted t1/t2, plus a hand-added id-less line.
        let mut f = TodoFile::parse(
            "\
a id:t1 branch:x
b id:t2 branch:x
c id:t1 branch:y
d id:t2 branch:y
e branch:x
",
        );
        f.heal_ids();
        let ids: Vec<&str> = f.tasks().map(|t| t.id().unwrap()).collect();
        // first t1/t2 kept; the duplicates and the id-less line get fresh ids past the max.
        assert_eq!(ids, vec!["t1", "t2", "t3", "t4", "t5"]);
        // deterministic — a second heal is a no-op.
        f.heal_ids();
        let again: Vec<&str> = f.tasks().map(|t| t.id().unwrap()).collect();
        assert_eq!(again, vec!["t1", "t2", "t3", "t4", "t5"]);
    }

    #[test]
    fn parse_does_not_heal_but_load_does() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TODO.txt");
        std::fs::write(&path, "a id:t1\nb id:t1\n").unwrap();
        // parse is verbatim (round-trip fidelity); load heals for addressability.
        assert_eq!(TodoFile::parse("a id:t1\nb id:t1\n").tasks().count(), 2);
        let loaded = load(&path).unwrap();
        let ids: Vec<&str> = loaded.tasks().map(|t| t.id().unwrap()).collect();
        assert_eq!(ids, vec!["t1", "t2"]);
    }

    #[test]
    fn persisting_a_healed_file_keeps_ids_stable_across_a_later_add() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TODO.txt");
        // A union-merge concatenated two branches that each minted t3.
        std::fs::write(&path, "a id:t1\nb id:t3\nc id:t3\n").unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut f = load(&path).unwrap();
        // c heals to t4 in memory, but the raw file still holds the dup — the render/raw mismatch
        // is the signal the CLI `add` path uses to persist the heal instead of blindly appending.
        assert_ne!(f.render(), raw);
        assert_eq!(f.tasks().nth(2).unwrap().id(), Some("t4"));
        // Persist the heal with a new task (as the add path now does), then reload: c stays t4
        // instead of being re-derived to a higher id — the shift the append-only path caused.
        let id = f.next_id();
        f.push_task(Task::new_todo(
            &id,
            "d",
            None,
            "main",
            "manual",
            "2026-07-04",
        ));
        save(&path, &f).unwrap();
        let reloaded = load(&path).unwrap();
        let ids: Vec<&str> = reloaded.tasks().map(|t| t.id().unwrap()).collect();
        assert_eq!(ids, vec!["t1", "t3", "t4", "t5"]);
    }

    #[test]
    fn append_task_preserves_existing_lines_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TODO.txt");
        // a hand-aligned line with irregular spacing, and no trailing newline
        std::fs::write(&path, "(A)   spaced   out   id:t1").unwrap();
        let t = Task::new_todo("t2", "new one", None, "x", "manual", "2026-07-04");
        append_task(&path, &t).unwrap();
        let on_disk = std::fs::read_to_string(&path).unwrap();
        // the original line is untouched (spacing intact); the new line is appended after a newline
        let (first, appended) = on_disk.split_once('\n').expect("two lines");
        assert_eq!(first, "(A)   spaced   out   id:t1");
        // `uid:`/`mod:` trail the stable meta (#995), so assert the prefix and the identity.
        assert!(
            appended.starts_with("2026-07-04 new one id:t2 branch:x src:manual uid:"),
            "unexpected appended line: {appended}"
        );
        assert!(appended.ends_with('\n'));
        assert_eq!(t.uid(), parse_task(appended.trim_end()).uid());
    }

    #[test]
    fn lock_key_is_stable_and_acquire_releases_on_drop() {
        // key is deterministic for the same path (so all processes contend on one lock)…
        let repo = tempfile::tempdir().unwrap();
        let path = repo.path().join("TODO.txt");
        assert_eq!(lock_key(&path), lock_key(&path));
        // …and differs per file.
        assert_ne!(lock_key(&path), lock_key(&repo.path().join("OTHER.txt")));

        // Acquire, drop (unlock), re-acquire in the same process: the second call must not
        // self-deadlock, which proves the guard releases the advisory lock on drop.
        let _g = crate::test_support::env_lock();
        let state = tempfile::tempdir().unwrap();
        let _state_env = crate::test_support::EnvGuard::set("BN_STATE_DIR", state.path());
        {
            let _l = lock(&path);
        }
        {
            let _l = lock(&path);
        }
    }

    #[test]
    fn append_task_creates_file_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TODO.txt");
        let t = Task::new_todo("t1", "first", Some('A'), "x", "manual", "2026-07-04");
        append_task(&path, &t).unwrap();
        let on_disk = std::fs::read_to_string(&path).unwrap();
        // `uid:`/`mod:` trail the stable meta (#995), so assert the prefix rather than the whole
        // line — the identity is minted fresh and the stamp is wall-clock.
        assert!(
            on_disk.starts_with("(A) 2026-07-04 first id:t1 branch:x src:manual uid:"),
            "unexpected line: {on_disk}"
        );
        assert!(on_disk.ends_with('\n'));
    }

    fn ing(text: &str, key: Option<&str>) -> IngestTodo {
        IngestTodo {
            text: text.into(),
            priority: None,
            severity: None,
            key: key.map(str::to_string),
            due: None,
            snooze: None,
            branch: None,
            src: None,
        }
    }

    #[test]
    fn ingest_errors_atomically_on_a_bad_row() {
        let mut f = TodoFile::default();
        f.add("existing", None, "main", "manual", "2026-07-04");
        let before = f.render();
        let good = ing("good row", Some("k1"));
        let mut bad = ing("bad date", Some("k2"));
        bad.due = Some("not-a-date".into());
        let err = f
            .ingest(&[good.clone(), bad, good], "s", "main", false, "2026-07-04")
            .unwrap_err();
        assert!(err.contains("invalid due"));
        // The good row before the bad one must NOT have been half-applied.
        assert_eq!(f.render(), before, "ingest half-applied before erroring");
    }

    #[test]
    fn parse_ingest_todos_accepts_array_object_and_jsonl_aliases() {
        let arr = parse_ingest_todos(
            r#"[{"title":"Review PR #51","key":"pr51","severity":"urgent","due":"2026-07-10"}]"#,
        );
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].text, "Review PR #51");
        assert_eq!(arr[0].severity.as_deref(), Some("urgent"));

        let one = parse_ingest_todos(r#"{"text":"One","key":"o"}"#);
        assert_eq!(one.len(), 1);

        let lines =
            parse_ingest_todos("{\"text\":\"A\"}\nnot json\n{\"title\":\"B\",\"src\":\"cron\"}");
        let texts: Vec<&str> = lines.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, vec!["A", "B"]);
    }

    #[test]
    fn ingest_is_idempotent_by_source_namespaced_key() {
        let mut f = TodoFile::default();
        let batch = vec![ing("Review PR #51", Some("pr51"))];
        let first = f
            .ingest(&batch, "pr-queue", "main", false, "2026-07-04")
            .unwrap();
        assert_eq!((first.added, first.skipped), (1, 0));
        // key is namespaced by source, and priority/severity absent ⇒ unprioritized
        let t = f.tasks().next().unwrap();
        assert_eq!(t.meta("key"), Some("pr-queue:pr51"));
        assert_eq!(t.meta("src"), Some("pr-queue"));

        // re-ingest the same open item → skipped, not duplicated
        let again = f
            .ingest(&batch, "pr-queue", "main", false, "2026-07-04")
            .unwrap();
        assert_eq!((again.added, again.skipped), (0, 1));
        assert_eq!(f.tasks().filter(|t| t.is_open("2026-07-04")).count(), 1);

        // a different source with the same natural key is a distinct item
        let other = f
            .ingest(
                &[ing("Review PR #51", Some("pr51"))],
                "manual",
                "main",
                false,
                "2026-07-04",
            )
            .unwrap();
        assert_eq!(other.added, 1);
    }

    #[test]
    fn ingest_replace_marks_absent_keyed_items_done() {
        let mut f = TodoFile::default();
        let full = vec![ing("PR #1", Some("p1")), ing("PR #2", Some("p2"))];
        f.ingest(&full, "pr-queue", "main", true, "2026-07-04")
            .unwrap();
        assert_eq!(f.open_sorted("2026-07-04", None).len(), 2);

        // next poll only reports p2 → p1 closes itself
        let out = f
            .ingest(
                &[ing("PR #2", Some("p2"))],
                "pr-queue",
                "main",
                true,
                "2026-07-04",
            )
            .unwrap();
        assert_eq!(out.resolved, 1);
        let open = f.open_sorted("2026-07-04", None);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].summary(), "PR #2");

        // empty replace clears the source entirely
        let out = f
            .ingest(&[], "pr-queue", "main", true, "2026-07-04")
            .unwrap();
        assert_eq!(out.resolved, 1);
        assert!(f.open_sorted("2026-07-04", None).is_empty());
    }

    #[test]
    fn ingest_maps_priority_and_validates() {
        let mut f = TodoFile::default();
        let mut item = ing("urgent work", None);
        item.severity = Some("urgent".into());
        f.ingest(&[item], "script", "main", false, "2026-07-04")
            .unwrap();
        assert_eq!(f.tasks().next().unwrap().priority, Some('A'));

        let mut bad = ing("nope", None);
        bad.priority = Some("Z".into());
        assert!(f
            .ingest(&[bad], "script", "main", false, "2026-07-04")
            .is_err());

        let mut baddate = ing("nope", None);
        baddate.due = Some("soon".into());
        assert!(f
            .ingest(&[baddate], "script", "main", false, "2026-07-04")
            .is_err());
    }

    #[test]
    fn to_json_exposes_folded_view() {
        let t = parse_task("(A) fix it @wip +bug due:2026-07-10 id:t1 branch:x src:agent");
        let v = t.to_json("2026-07-03");
        assert_eq!(v["id"], "t1");
        assert_eq!(v["status"], "wip");
        assert_eq!(v["priority"], "A");
        assert_eq!(v["summary"], "fix it @wip +bug");
        assert_eq!(v["due"], "2026-07-10");
        assert_eq!(v["src"], "agent");
    }

    // ---- #995: surviving a union merge -------------------------------------------------

    /// One task line as two machines would each have rewritten it: same `uid:`, different `mod:`.
    fn line(id: &str, uid: &str, modified: i64, text: &str) -> String {
        format!("2026-08-03 {text} id:{id} uid:{uid} mod:{modified}")
    }

    #[test]
    fn new_tasks_are_born_with_an_identity_and_a_stamp() {
        let t = Task::new_todo("t1", "fix the thing", None, "main", "manual", "2026-08-03");
        assert!(t.uid().is_some_and(|u| !u.is_empty()));
        assert!(t.modified() > 0);
    }

    #[test]
    fn distinct_tasks_never_share_an_identity() {
        let a = Task::new_todo("t1", "same text", None, "main", "manual", "2026-08-03");
        let b = Task::new_todo("t2", "same text", None, "main", "manual", "2026-08-03");
        assert_ne!(a.uid(), b.uid(), "identical text must not collide");
    }

    /// The #995 acceptance case: both machines rewrote the file, git concatenated both copies,
    /// and the load must come back with the original task count — each at its newest version.
    #[test]
    fn union_merged_rewrites_collapse_to_the_newest_copy() {
        let merged = format!(
            "{}\n{}\n{}\n{}\n",
            line("t1", "aaa", 100, "ship the parser"),
            line("t2", "bbb", 100, "write the docs"),
            // …the same two tasks again, as the other machine last wrote them.
            line("t1", "aaa", 200, "ship the parser @wip"),
            line("t2", "bbb", 50, "write the docs"),
        );
        let mut f = TodoFile::parse(&merged);
        let removed = f.dedupe_by_uid();

        assert_eq!(removed, 2);
        assert_eq!(f.tasks().count(), 2, "original task count restored");
        let by_uid = |uid: &str| {
            f.tasks()
                .find(|t| t.uid() == Some(uid))
                .expect("task survived")
                .clone()
        };
        assert_eq!(by_uid("aaa").modified(), 200, "newest copy wins");
        assert!(by_uid("aaa").contexts().contains(&"wip"));
        assert_eq!(by_uid("bbb").modified(), 100, "newest copy wins");
    }

    /// The case a naive `dedupe_by_id` would silently destroy: two machines offline each minted
    /// `t6` for a *different* task. They are not duplicates — both must survive, renumbered.
    #[test]
    fn two_tasks_that_collided_on_a_handle_both_survive() {
        let merged = format!(
            "{}\n{}\n",
            line("t6", "aaa", 100, "fix the login bug"),
            line("t6", "bbb", 100, "add the export button"),
        );
        let mut f = TodoFile::parse(&merged);

        assert_eq!(f.dedupe_by_uid(), 0, "distinct tasks are not duplicates");
        f.heal_ids();

        assert_eq!(f.tasks().count(), 2);
        let ids: Vec<&str> = f.tasks().filter_map(|t| t.id()).collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1], "the handle collision was renumbered");
        let summaries: Vec<String> = f.tasks().map(|t| t.summary()).collect();
        assert!(summaries.iter().any(|s| s.contains("login")));
        assert!(summaries.iter().any(|s| s.contains("export")));
    }

    /// Dedupe must run before healing. Reversed, healing renumbers one copy of a duplicated task
    /// and the duplicate becomes permanent — so assert the pipeline `load` actually uses.
    #[test]
    fn dedupe_runs_before_healing() {
        let merged = format!(
            "{}\n{}\n",
            line("t1", "aaa", 100, "ship the parser"),
            line("t1", "aaa", 200, "ship the parser @wip"),
        );

        let mut correct = TodoFile::parse(&merged);
        correct.dedupe_by_uid();
        correct.heal_ids();
        assert_eq!(correct.tasks().count(), 1);

        // The wrong order, spelled out: healing first mints a fresh id for the second copy, and
        // dedupe can still see the shared uid — but the file has already been renumbered, which
        // is what makes the duplicate look like real work to everything downstream.
        let mut wrong = TodoFile::parse(&merged);
        wrong.heal_ids();
        let ids: Vec<String> = wrong
            .tasks()
            .filter_map(|t| t.id().map(str::to_string))
            .collect();
        assert_ne!(ids[0], ids[1], "healing first renumbers a true duplicate");
    }

    #[test]
    fn lines_without_an_identity_are_left_alone() {
        // Hand-added / pre-#995 lines have no `uid:` — there is nothing to match on, so dedupe
        // must not guess. `heal_ids` still gives them addressable handles.
        let merged = "2026-08-03 hand written note\n2026-08-03 hand written note\n";
        let mut f = TodoFile::parse(merged);
        assert_eq!(f.dedupe_by_uid(), 0);
        assert_eq!(f.tasks().count(), 2);
        f.heal_ids();
        assert!(f.tasks().all(|t| t.id().is_some()));
    }

    #[test]
    fn an_unstamped_copy_loses_to_a_stamped_one() {
        let merged = format!(
            "2026-08-03 ship it id:t1 uid:aaa\n{}\n",
            line("t1", "aaa", 500, "ship it @wip"),
        );
        let mut f = TodoFile::parse(&merged);
        f.dedupe_by_uid();
        assert_eq!(f.tasks().count(), 1);
        assert_eq!(f.tasks().next().unwrap().modified(), 500);
    }

    #[test]
    fn dedupe_is_order_independent_and_deterministic() {
        let a = line("t1", "aaa", 100, "ship it");
        let b = line("t1", "aaa", 200, "ship it @wip");
        let mut forward = TodoFile::parse(&format!("{a}\n{b}\n"));
        let mut backward = TodoFile::parse(&format!("{b}\n{a}\n"));
        forward.dedupe_by_uid();
        backward.dedupe_by_uid();
        assert_eq!(forward.tasks().next().unwrap().modified(), 200);
        assert_eq!(backward.tasks().next().unwrap().modified(), 200);
    }

    #[test]
    fn mutations_stamp_mod_but_healing_does_not() {
        let mut t = Task::new_todo("t1", "ship it", None, "main", "manual", "2026-08-03");
        t.set_meta_raw(MOD_KEY, "1000");

        t.set_meta("due", "2026-08-10");
        assert!(t.modified() > 1000, "a user edit stamps mod:");

        t.set_meta_raw(MOD_KEY, "1000");
        t.set_meta_raw("id", "t9");
        assert_eq!(t.modified(), 1000, "healing an id must not stamp mod:");
    }

    #[test]
    fn marking_done_and_wip_stamps_mod() {
        let mut t = Task::new_todo("t1", "ship it", None, "main", "manual", "2026-08-03");
        t.set_meta_raw(MOD_KEY, "1000");
        t.mark_wip();
        assert!(t.modified() > 1000);

        t.set_meta_raw(MOD_KEY, "1000");
        t.mark_done("2026-08-04");
        assert!(t.modified() > 1000);
    }

    #[test]
    fn load_repairs_a_union_merged_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.txt");
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n{}\n",
                line("t1", "aaa", 100, "ship the parser"),
                line("t1", "aaa", 200, "ship the parser @wip"),
                line("t2", "bbb", 100, "write the docs"),
            ),
        )
        .unwrap();

        let f = load(&path).unwrap();
        assert_eq!(f.tasks().count(), 2);
        assert_eq!(
            f.tasks()
                .find(|t| t.uid() == Some("aaa"))
                .unwrap()
                .modified(),
            200
        );
    }

    #[test]
    fn ensure_union_merge_is_idempotent_and_additive() {
        let dir = tempfile::tempdir().unwrap();
        let ga = dir.path().join(".gitattributes");
        std::fs::write(&ga, "**/events.jsonl merge=union\n").unwrap();

        ensure_union_merge(dir.path()).unwrap();
        ensure_union_merge(dir.path()).unwrap();

        let content = std::fs::read_to_string(&ga).unwrap();
        assert_eq!(
            content.matches("tasks.txt merge=union").count(),
            1,
            "written exactly once: {content}"
        );
        assert!(
            content.contains("**/events.jsonl merge=union"),
            "existing rules preserved: {content}"
        );
    }

    #[test]
    fn tasks_paths_are_person_scoped() {
        // Person-scoped, not under `<machine>/` — a task list that differs per laptop would not
        // be a single source of truth (#995).
        let cfg = crate::resolver::Config {
            personal_root: std::path::PathBuf::from("/notes"),
            hostname: "somebox".into(),
            ..Default::default()
        };
        assert_eq!(cfg.tasks_file(), std::path::Path::new("/notes/tasks.txt"));
        assert_eq!(cfg.tasks_dir(), std::path::Path::new("/notes/tasks"));
    }
}
