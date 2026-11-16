//! Task rollups and reference search.
use crate::*;

/// Bound each task summary so captured prompts cannot bury the other rows.
const SUMMARY_WIDTH: usize = 110;

pub(crate) fn emit_summary(cfg: &Config, as_json: bool) {
    let entries = rollups::summary(cfg);
    if as_json && json_ok("summary", payload::summary_data(&entries)) {
        return;
    }
    if entries.is_empty() {
        println!("No tasks found");
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

