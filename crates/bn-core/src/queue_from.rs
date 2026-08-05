//! Build an orchestrate work queue from a failing check's output (#609).
//!
//! Inspired by Bun's Rust rewrite: a `cargo check` that emits thousands of errors is *grouped* by
//! file/dir and *sharded* into a work queue, then each shard is driven to green by its own agent —
//! no human hand-listing issue numbers. This module is the pure, unit-tested brain: it parses a
//! check's stdout+stderr into per-file [`Diagnostic`]s and folds them into [`QueueItem`]s (one work
//! item per group) whose `touches` globs let the conflict-aware scheduler
//! ([`crate::orchestrate::touches_conflict`]) keep two agents off the same file. The CLI
//! (`bn orchestrate queue-from`) only runs the check and appends the items to the durable backlog.
//!
//! Two diagnostic shapes are recognized, covering the common toolchains:
//! * **rustc / clippy** — a `error[E0308]: …` / `warning: …` header followed by a ` --> path:line:col`
//!   location line (the location carries the file; the header carries the message).
//! * **generic `path:line[:col]: message`** — one-line diagnostics as emitted by tsc, eslint,
//!   gcc/clang, shellcheck, ripgrep, and most POSIX linters.

use crate::orchestrate::derive_id;
use std::collections::BTreeMap;

/// One parsed failure: the source file it points at and the (best-effort) message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// The file path exactly as the tool printed it (normalized to forward slashes).
    pub file: String,
    /// The diagnostic message, trimmed. Empty when the tool printed only a location.
    pub message: String,
}

/// How to shard the diagnostics into work items — the grouping axis (#609 "by file/crate/module").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupBy {
    /// One work item per file — the finest, most-parallel sharding (the default).
    File,
    /// One work item per parent directory (≈ per module/crate) — coarser, fewer lanes.
    Dir,
}

impl GroupBy {
    /// Parse a kebab-case label (`file` / `dir`), returning `None` for anything unrecognized.
    pub fn parse(s: &str) -> Option<GroupBy> {
        match s.trim().to_ascii_lowercase().as_str() {
            "file" => Some(GroupBy::File),
            "dir" | "directory" | "module" | "crate" => Some(GroupBy::Dir),
            _ => None,
        }
    }
}

/// One shard of the queue: a group of failures that a single agent will fix, plus the `touches`
/// glob that keeps any other lane off the same area while it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueItem {
    /// The backlog subject text — slugifies (via [`derive_id`]) to a unique id within the batch.
    pub title: String,
    /// The path globs this shard is expected to touch — the overlap-aware scheduling hint.
    pub touches: Vec<String>,
    /// The distinct files in this shard (for the summary / `--json`).
    pub files: Vec<String>,
    /// How many diagnostics landed in this shard.
    pub count: usize,
}

/// Normalize a printed path: strip a leading `./`, collapse backslashes to `/`, trim surrounding
/// whitespace/quotes. Kept coarse — it only needs to be stable enough to group and to glob-match.
fn norm_path(raw: &str) -> String {
    let t = raw.trim().trim_matches(['"', '\'', '`']).trim();
    let t = t.replace('\\', "/");
    t.strip_prefix("./").unwrap_or(&t).to_string()
}

/// Whether `s` names a file we can meaningfully attribute a fix to (has an extension segment and no
/// spaces). Guards the generic parser against matching prose like `note: see issue: 5`.
fn looks_like_path(s: &str) -> bool {
    !s.is_empty()
        && !s.contains(char::is_whitespace)
        && s.rsplit('/').next().is_some_and(|base| base.contains('.'))
}

/// Parse a ` --> path:line:col` rustc/clippy location line into its file, if it is one.
fn parse_rustc_location(line: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix("-->")?.trim();
    // `path:line:col` — take everything before the first `:line` numeric segment.
    let file = rest.split(':').next().unwrap_or(rest);
    let file = norm_path(file);
    looks_like_path(&file).then_some(file)
}

/// Parse a generic `path:line[:col]: message` diagnostic into (file, message), if it is one.
fn parse_generic(line: &str) -> Option<(String, String)> {
    let mut it = line.splitn(2, ':');
    let file = norm_path(it.next()?);
    if !looks_like_path(&file) {
        return None;
    }
    let rest = it.next()?; // `line[:col]: message`
    let mut segs = rest.splitn(2, ':');
    // Require a leading numeric line number so we don't match `http://…` or prose colons.
    let line_no = segs.next()?.trim();
    if line_no.is_empty() || !line_no.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let after_line = segs.next().unwrap_or("").trim(); // `col: message` or `message`
                                                       // A leading numeric segment is an optional column — strip it off the message.
    let message = match after_line.split_once(':') {
        Some((col, msg))
            if !col.trim().is_empty() && col.trim().chars().all(|c| c.is_ascii_digit()) =>
        {
            msg.trim().to_string()
        }
        _ => after_line.to_string(),
    };
    Some((file, message))
}

/// Strip a rustc/clippy header line (`error[E0308]: …` / `warning: …`) to its message, if it is one.
fn parse_rustc_header(line: &str) -> Option<String> {
    let t = line.trim_start();
    for prefix in ["error", "warning"] {
        if let Some(rest) = t.strip_prefix(prefix) {
            // Accept `error:`, `error[code]:`, `warning:` — a `[code]` then a colon.
            let after_code = rest
                .strip_prefix('[')
                .map_or(rest, |r| r.split_once(']').map(|(_, x)| x).unwrap_or(r));
            if let Some(msg) = after_code.strip_prefix(':') {
                return Some(msg.trim().to_string());
            }
        }
    }
    None
}

/// Parse a check's combined output into per-file [`Diagnostic`]s. Recognizes rustc/clippy
/// header+location pairs and one-line `path:line:col: message` diagnostics; unrecognized lines are
/// ignored. Order-preserving and pure.
pub fn parse_diagnostics(output: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let mut pending_msg: Option<String> = None;
    for line in output.lines() {
        // rustc/clippy: a header primes the message for the next location line.
        if let Some(msg) = parse_rustc_header(line) {
            pending_msg = Some(msg);
            continue;
        }
        if let Some(file) = parse_rustc_location(line) {
            out.push(Diagnostic {
                file,
                message: pending_msg.take().unwrap_or_default(),
            });
            continue;
        }
        // generic one-liners (tsc/eslint/gcc/shellcheck/…).
        if let Some((file, message)) = parse_generic(line) {
            out.push(Diagnostic { file, message });
        }
    }
    out
}

/// The grouping key for a file under `group_by` — the file itself, or its parent directory.
fn group_key(file: &str, group_by: GroupBy) -> String {
    match group_by {
        GroupBy::File => file.to_string(),
        GroupBy::Dir => match file.rsplit_once('/') {
            Some((dir, _)) => dir.to_string(),
            None => ".".to_string(),
        },
    }
}

/// The `touches` glob a shard declares for `key` under `group_by`: the file exactly, or `dir/**`.
fn touches_for(key: &str, group_by: GroupBy) -> Vec<String> {
    match group_by {
        GroupBy::File => vec![key.to_string()],
        GroupBy::Dir => vec![format!("{}/**", key.trim_end_matches('/'))],
    }
}

/// Build a concise, agent-facing title for a shard: leads with the touch area (so its slugged id is
/// distinctive) then the check + a couple of sample messages.
fn build_title(key: &str, check: &str, files: &[String], msgs: &[String], count: usize) -> String {
    let noun = if count == 1 { "finding" } else { "findings" };
    let scope = if files.len() > 1 {
        format!("{} ({} files)", key, files.len())
    } else {
        key.to_string()
    };
    // A short label for the check — the full command lives on the entry's `source`, not the title.
    let label: String = {
        let one_line = check.split('\n').next().unwrap_or(check).trim();
        if one_line.chars().count() > 48 {
            format!("{}…", one_line.chars().take(47).collect::<String>())
        } else {
            one_line.to_string()
        }
    };
    let mut title = format!("{scope}: fix {count} `{label}` {noun}");
    let sample: Vec<&str> = msgs
        .iter()
        .filter(|m| !m.is_empty())
        .take(2)
        .map(|s| s.as_str())
        .collect();
    if !sample.is_empty() {
        title.push_str(" — ");
        title.push_str(&sample.join("; "));
    }
    // Keep the backlog line to a sane length.
    if title.chars().count() > 200 {
        let cut: String = title.chars().take(197).collect();
        title = format!("{cut}…");
    }
    title
}

/// Fold parsed diagnostics into work items grouped by `group_by`, one shard per group, ordered by
/// diagnostic count (busiest first) then group key. Each item's `touches` isolates its area and its
/// `title` is guaranteed to slugify to a **unique** id within the batch (a rare collision — two
/// groups whose first path segments coincide — gets a short `gN` disambiguator prefix). Pure.
pub fn build_queue(check: &str, output: &str, group_by: GroupBy) -> Vec<QueueItem> {
    let diags = parse_diagnostics(output);
    // key -> (ordered distinct files, ordered distinct messages, count)
    let mut groups: BTreeMap<String, (Vec<String>, Vec<String>, usize)> = BTreeMap::new();
    for d in &diags {
        let key = group_key(&d.file, group_by);
        let entry = groups.entry(key).or_default();
        if !entry.0.contains(&d.file) {
            entry.0.push(d.file.clone());
        }
        if !d.message.is_empty() && !entry.1.contains(&d.message) {
            entry.1.push(d.message.clone());
        }
        entry.2 += 1;
    }

    let mut items: Vec<QueueItem> = groups
        .into_iter()
        .map(|(key, (files, msgs, count))| QueueItem {
            title: build_title(&key, check, &files, &msgs, count),
            touches: touches_for(&key, group_by),
            files,
            count,
        })
        .collect();

    // Busiest shard first, then stable by key (via touches) for determinism.
    items.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.touches.cmp(&b.touches))
    });

    // Guarantee unique derived ids: derive_id only sees the first ~6 slug words, so two groups
    // sharing them would dedup to one backlog entry and silently drop a shard. Prefix a `gN` token
    // (which leads the slug) on the rare collision.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut n = 1;
    for item in &mut items {
        while !seen.insert(derive_id(&item.title)) {
            item.title = format!("g{n} {}", item.title);
            n += 1;
        }
    }
    items
}
