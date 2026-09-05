//! `bn recall / search / related / todo / list / summary` — semantic recall + note listings.
use crate::*;

/// Auto-surface (issue #249): on a freshly-created note, print up to 3 semantically-similar past
/// notes for the branch/goal to stderr — "have I thought about this before?". Read-only against a
/// warm index: if the index is cold it prints nothing (never blocks note creation on embedding).
pub(crate) fn surface_recall_on_new_note(cfg: &Config, ctx: &bn_core::resolver::Resolved) {
    use bn_core::recall;
    // Query = the branch name (slashes/dashes → words) plus the goal line if one's already set.
    let goal = bn_core::store::read(&cfg.note_file(ctx))
        .ok()
        .and_then(|n| bn_core::render::goal_text(&n.body));
    let branch_words = ctx.branch.replace(['-', '/', '_'], " ");
    let query = match &goal {
        Some(g) if !g.trim().is_empty() => format!("{branch_words} {g}"),
        _ => branch_words,
    };
    let hits = recall::recall_peek(
        &recall::default_index_path(),
        &query,
        &ctx.repo,
        &ctx.branch,
        3,
        &today(),
        &recall::default_embedder(),
    );
    if hits.is_empty() {
        return;
    }
    eprintln!("\nsimilar past work:");
    for h in &hits {
        let c = &h.chunk;
        let status = if c.status == "closed" {
            "closed"
        } else {
            "active"
        };
        let snippet: String = c.text.replace('\n', " ").trim().chars().take(100).collect();
        eprintln!(
            "  {}/{} ({}) — {}: {}",
            c.repo, c.branch, status, c.kind, snippet
        );
    }
}

pub(crate) fn emit_todo(cfg: &Config, as_json: bool) {
    let entries = rollups::todos(cfg);
    if as_json && json_ok("todo", payload::todos_data(&entries)) {
        return;
    }
    if entries.is_empty() {
        println!("No open todos");
        return;
    }
    for e in entries {
        match e.pct {
            Some(p) => println!("{}/{}  [{}] {} ({p}%)", e.repo, e.branch, e.id, e.text),
            None => println!("{}/{}  [{}] {}", e.repo, e.branch, e.id, e.text),
        }
    }
}

pub(crate) fn emit_list(cfg: &Config, as_json: bool) {
    let entries = rollups::list(cfg);
    if as_json && json_ok("list", payload::list_data(&entries)) {
        return;
    }
    if entries.is_empty() {
        println!("No branch notes found");
        return;
    }
    for e in entries {
        // A blocker is the only thing in this row that can be wrong, so it is the only thing
        // that earns colour; the rest is metadata you scan past.
        let counts = format!("{} todos, {} blockers", e.open_todos, e.blockers);
        let counts = if e.blockers > 0 {
            style::warn(&counts)
        } else {
            style::meta(&counts)
        };
        let status = style::meta(&format!("({})", e.status));
        println!("{}/{}  {counts}  {status}", e.repo, e.branch);
    }
}

/// How much of a note item `bn summary` shows before deferring to `bn cat`. Wide enough for a real
/// todo, narrow enough that a dispatch prompt stored as a todo cannot bury the rows under it.
const SUMMARY_WIDTH: usize = 110;

pub(crate) fn emit_summary(cfg: &Config, as_json: bool) {
    let entries = rollups::summary(cfg);
    if as_json && json_ok("summary", payload::summary_data(&entries)) {
        return;
    }
    if entries.is_empty() {
        println!("No branch notes found");
        return;
    }
    for e in entries {
        if e.todos.is_empty() && e.blockers.is_empty() {
            continue;
        }
        println!(
            "{}/{} {}",
            e.repo,
            e.branch,
            style::meta(&format!("({})", e.status))
        );
        for b in &e.blockers {
            println!(
                "  {} {} {}",
                style::danger("blocker"),
                style::meta(&format!("[{}]", b.id)),
                bn_core::render::oneline(&b.text, SUMMARY_WIDTH)
            );
        }
        for t in &e.todos {
            println!(
                "  [ ] {} {}",
                style::meta(&format!("[{}]", t.id)),
                bn_core::render::oneline(&t.text, SUMMARY_WIDTH)
            );
        }
    }
}

/// `bn recall [query...] [--repo R] [--index] [--limit N]` — semantic recall over the whole note
/// corpus. `--index` (re)builds the machine-local index; otherwise the query is ranked against a
/// warm index, degrading to `bn search` (with a stderr notice) when the index is cold/stale.
pub(crate) fn emit_recall(
    cfg: &Config,
    query: &str,
    repo: Option<&str>,
    index: bool,
    limit: usize,
    as_json: bool,
) {
    use bn_core::recall;
    let index_path = recall::default_index_path();
    let embedder = recall::default_embedder();

    if index {
        let stats = recall::build_index(cfg, &index_path, &embedder);
        if as_json && json_ok("recall", payload::recall_index_data(&stats)) {
            return;
        }
        let mode = if stats.full_rebuild {
            "full rebuild"
        } else {
            "incremental"
        };
        println!(
            "recall index ({mode}): {} files, {} chunks — {} embedded, {} reused\n  {}",
            stats.files,
            stats.chunks,
            stats.embedded,
            stats.reused,
            index_path.display()
        );
        return;
    }

    if query.trim().is_empty() {
        fail("bn recall: give a query, or `--index` to (re)build the index");
    }

    let out = recall::recall(cfg, &index_path, query, repo, limit, &today(), &embedder);
    if as_json && json_ok("recall", payload::recall_data(&out)) {
        return;
    }
    if let Some(reason) = &out.degraded_reason {
        eprintln!("bn recall: {reason}");
    }
    if out.hits.is_empty() {
        println!("No recall hits for '{query}'");
        return;
    }
    for h in &out.hits {
        let c = &h.chunk;
        let archived = if c.status == "closed" {
            " (archived)"
        } else {
            ""
        };
        let date = if c.date.is_empty() {
            String::new()
        } else {
            format!(" {}", c.date)
        };
        // Score is meaningful only for real index hits (fallback hits are all 0.0).
        let score = if out.source == recall::RecallSource::Index {
            format!("  [{:.2}] ", h.score)
        } else {
            "  ".to_string()
        };
        let snippet = c.text.replace('\n', " ");
        let snippet = snippet.trim();
        println!(
            "{}/{} {}{}{}\n{}{}",
            c.repo, c.branch, c.kind, date, archived, score, snippet
        );
    }
}

pub(crate) fn emit_search(cfg: &Config, query: &str, limit: usize, as_json: bool) {
    let hits = bn_core::search::search(cfg, query, limit);
    if as_json && json_ok("search", payload::search_data(&hits)) {
        return;
    }
    if hits.is_empty() {
        println!("No matches for '{query}'");
        return;
    }
    for h in &hits {
        let archived = if h.status == "closed" {
            " (archived)"
        } else {
            ""
        };
        println!(
            "{}/{} {}:{}{}\n  {}",
            h.repo, h.branch, h.kind, h.line, archived, h.snippet
        );
    }
}

/// `bn related [--branch <name>] [--limit N]` — past branches that touched the same files,
/// ranked by overlap. Derived offline from git via the command-runner seam.
pub(crate) fn emit_related(cfg: &Config, branch: Option<&str>, limit: usize, as_json: bool) {
    use bn_core::runner::{CommandRunner, RealRunner};
    // Resolve the target branch ONCE, so the JSON `branch` label and the scoring are computed
    // against the same ref — resolving twice lets a mid-flight branch switch yield
    // {branch: A, related: scored against B}.
    let target = branch.map(str::to_string).unwrap_or_else(|| {
        RealRunner
            .git(&["rev-parse", "--abbrev-ref", "HEAD"])
            .ok_stdout()
            .filter(|b| b != "HEAD")
            .unwrap_or_default()
    });
    // Empty target = detached HEAD; pass None so related_branches keeps its "yields nothing" path.
    let target_opt = (!target.is_empty()).then_some(target.as_str());
    let related = bn_core::related::related_branches(cfg, &RealRunner, target_opt, limit);
    if as_json && json_ok("related", payload::related_data(&target, &related)) {
        return;
    }
    if related.is_empty() {
        println!("No related branches — nothing else touched {target}'s files");
        return;
    }
    let label = if target.is_empty() {
        "this branch".to_string()
    } else {
        target
    };
    println!("Past branches that touched {label}'s files:");
    for r in &related {
        let area = if r.work_item_match {
            " · same area"
        } else {
            ""
        };
        let dec = if r.decisions > 0 {
            format!(
                " · {} decision{}",
                r.decisions,
                if r.decisions == 1 { "" } else { "s" }
            )
        } else {
            String::new()
        };
        let archived = if r.status == "closed" {
            " (archived)"
        } else {
            ""
        };
        let files = r.shared_files.join(", ");
        println!(
            "  {}/{}{}  {} shared{}{}\n    {}",
            r.repo, r.branch, archived, r.shared_count, area, dec, files
        );
    }
}
