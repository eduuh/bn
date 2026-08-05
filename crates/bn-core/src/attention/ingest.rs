//! Raising items: `add` and friends, script `ingest` batches, and promoting a
//! branch todo onto the attention list.

use super::lifecycle::{ack, mark_planned, update};
use super::model::{
    AddOutcome, IngestItem, IngestOutcome, IngestedItem, PlanError, PlanOutcome, SEVERITIES,
};
use super::query::{live_by_key, open_items};
use super::store::{get, next_id};
use crate::commands;
use crate::events;
use crate::resolver::{self, Config, Resolved};
use serde_json::{json, Map};
use std::path::Path;

/// The fields for raising an attention item — see [`add_new`]. Carried as a struct so a new
/// optional field (`mode`, `prompt`, …) is one more struct field, not another positional argument
/// on an ever-growing signature. The thin [`add`]/[`add_with_detail`] wrappers and the CLI build it.
#[derive(Debug, Default, Clone)]
pub struct NewAttn<'a> {
    pub source: &'a str,
    pub title: &'a str,
    pub severity: &'a str,
    pub repo: Option<&'a str>,
    pub branch: Option<&'a str>,
    pub link: Option<&'a str>,
    pub key: Option<&'a str>,
    pub detail: Option<&'a str>,
    pub mode: Option<&'a str>,
    pub prompt: Option<&'a str>,
    /// Which pipeline stage the item belongs to (`coding`/`planning`/`critique`/`review`). `None`
    /// folds to `coding`. Written as the `task_type` event field, read back by `fold_item`.
    pub task_type: Option<&'a str>,
    /// Absolute path of the worktree a `wt-stuck` item is about (structured locator).
    pub worktree_path: Option<&'a str>,
    /// The uncommitted file paths blocking a merged worktree's reclaim — the actionable list a
    /// consumer renders instead of re-parsing the change-count sentence. Empty for other items.
    pub uncommitted_files: Vec<String>,
}

/// Raise an attention item from a [`NewAttn`]. With a `key`, the add is idempotent: if a *live*
/// (non-terminal) item already carries that key this refreshes it in place and returns the existing
/// id, so a re-firing cron job keeps one current record rather than piling up duplicates. Returns
/// the (new or existing) id.
///
/// Dedup scans every live item — the actionable lane, the completion/review lane, *and* items that
/// were started/done/snoozed out of the open lane ([`live_by_key`]) — so a re-fired keyed job (a
/// `wt-stuck` notice, a `bn agent done`, …) matches its own earlier item regardless of that item's
/// current phase or the day it was raised, instead of duplicating it (#534). Only a terminal
/// ack/remove/planned frees the key for a genuinely fresh occurrence. `today` is unused now that
/// dedup is date-independent; it is kept for signature stability across the many callers.
pub fn add_new(n: &NewAttn, today: &str) -> std::io::Result<AddOutcome> {
    let _ = today;
    if let Some(k) = n.key {
        if let Some(existing) = live_by_key(k) {
            // Refresh the one live record in place — its text/severity may have moved on (e.g. the
            // uncommitted-change count in a `wt-stuck` notice) — but only write when something
            // actually changed, so an unchanged re-fire stays a pure no-op.
            let title = (existing.title != n.title).then_some(n.title);
            let severity = (existing.severity != n.severity).then_some(n.severity);
            let detail = n.detail.filter(|d| existing.detail.as_deref() != Some(*d));
            if title.is_some() || severity.is_some() || detail.is_some() {
                update(
                    &existing.id,
                    title,
                    severity,
                    None,
                    None,
                    None,
                    detail,
                    None,
                    None,
                )?;
            }
            // The structured worktree fields aren't part of `update`'s signature, so overlay them
            // with a focused `attn_update` when a re-fire's path/dirty set has actually moved —
            // keeping the actionable list in step with the (already-refreshed) title's count.
            let path_changed =
                n.worktree_path.is_some() && existing.worktree_path.as_deref() != n.worktree_path;
            let files_changed = !n.uncommitted_files.is_empty()
                && existing.uncommitted_files != n.uncommitted_files;
            if path_changed || files_changed {
                let mut ov = Map::new();
                ov.insert("ref".into(), json!(existing.id));
                if path_changed {
                    if let Some(path) = n.worktree_path {
                        ov.insert("worktree_path".into(), json!(path));
                    }
                }
                if files_changed {
                    ov.insert("uncommitted_files".into(), json!(n.uncommitted_files));
                }
                events::append("attn_update", ov)?;
            }
            return Ok(AddOutcome {
                id: existing.id,
                skipped: true,
            });
        }
    }

    let id = next_id();
    let mut fields = Map::new();
    fields.insert("id".into(), json!(id));
    fields.insert("source".into(), json!(n.source));
    fields.insert("title".into(), json!(n.title));
    fields.insert("severity".into(), json!(n.severity));
    for (k, v) in [
        ("repo", n.repo),
        ("branch", n.branch),
        ("link", n.link),
        ("key", n.key),
        ("detail", n.detail),
        ("mode", n.mode),
        ("prompt", n.prompt),
        ("task_type", n.task_type),
    ] {
        if let Some(val) = v {
            fields.insert(k.into(), json!(val));
        }
    }
    if let Some(p) = n.worktree_path {
        fields.insert("worktree_path".into(), json!(p));
    }
    if !n.uncommitted_files.is_empty() {
        fields.insert("uncommitted_files".into(), json!(n.uncommitted_files));
    }
    events::append("attn", fields)?;
    Ok(AddOutcome { id, skipped: false })
}

/// Raise a plain attention item — the common one-line case (no detail/mode/prompt).
#[allow(clippy::too_many_arguments)]
pub fn add(
    source: &str,
    title: &str,
    severity: &str,
    repo: Option<&str>,
    branch: Option<&str>,
    link: Option<&str>,
    key: Option<&str>,
    today: &str,
) -> std::io::Result<AddOutcome> {
    add_new(
        &NewAttn {
            source,
            title,
            severity,
            repo,
            branch,
            link,
            key,
            ..Default::default()
        },
        today,
    )
}

/// As [`add`], plus rich multi-line `detail` for the detail/second view (e.g. a review's full
/// reasoning). The list view still shows only `title`.
#[allow(clippy::too_many_arguments)]
pub fn add_with_detail(
    source: &str,
    title: &str,
    severity: &str,
    repo: Option<&str>,
    branch: Option<&str>,
    link: Option<&str>,
    key: Option<&str>,
    today: &str,
    detail: Option<&str>,
) -> std::io::Result<AddOutcome> {
    add_new(
        &NewAttn {
            source,
            title,
            severity,
            repo,
            branch,
            link,
            key,
            detail,
            ..Default::default()
        },
        today,
    )
}

/// Promote attention item `id` to a `section` item and resolve it off the list. The todo lands
/// on the item's own repo/branch when it carries both, else on the note resolved from `base_dir`.
pub fn plan_item(
    cfg: &Config,
    base_dir: &Path,
    id: &str,
    section: &str,
    today: &str,
) -> Result<PlanOutcome, PlanError> {
    let item = get(id).ok_or(PlanError::NoItem)?;
    let (repo, branch) = match (&item.repo, &item.branch) {
        (Some(r), Some(b)) => (Some(r.clone()), Some(b.clone())),
        _ => (None, None),
    };
    let ctx = resolver::resolve_target(cfg, base_dir, repo, branch).map_err(PlanError::NoTarget)?;
    let text = match &item.link {
        Some(l) => format!("{} — {}", item.title, l),
        None => item.title.clone(),
    };
    let todo = commands::add(cfg, &ctx, section, &text, None, today).map_err(PlanError::Add)?;
    mark_planned(id, &todo.id).map_err(PlanError::Resolve)?;
    Ok(PlanOutcome { todo, ctx })
}

/// Outcome of promoting a branch todo into an attention item — the inverse of [`plan_item`].
#[derive(Debug)]
pub struct PromoteOutcome {
    pub add: AddOutcome,
    pub ctx: Resolved,
    pub todo_text: String,
}

/// Why promoting a todo to the attention list failed.
#[derive(Debug)]
pub enum PromoteError {
    NoTarget(resolver::ContextError),
    Read(crate::store::StoreError),
    NoTodo,
    TodoDone,
    Write(std::io::Error),
}

/// Promote branch todo `todo_id` into an attention item — the inverse of [`plan_item`], turning
/// existing note work into loop fuel. The note is `repo`/`branch` when both are given, else the
/// one resolved from `base_dir` (same targeting rule as `plan_item`). The prompt is seeded terse
/// (goal + todo text) because the launched agent reads the full note itself (`bn --cat`). Keyed
/// `attn:promote:<repo>:<branch>:<todo-id>`, so re-promoting an already-queued todo is a no-op
/// instead of a duplicate. The todo itself stays open — the agent working the branch checks it
/// off as part of the work.
#[allow(clippy::too_many_arguments)]
pub fn promote_todo(
    cfg: &Config,
    base_dir: &Path,
    repo: Option<String>,
    branch: Option<String>,
    todo_id: &str,
    mode: &str,
    severity: &str,
    today: &str,
) -> Result<PromoteOutcome, PromoteError> {
    let ctx =
        resolver::resolve_target(cfg, base_dir, repo, branch).map_err(PromoteError::NoTarget)?;
    let note = crate::store::read(&cfg.note_file(&ctx)).map_err(PromoteError::Read)?;
    let todo = note
        .front
        .todos
        .iter()
        .find(|t| t.id == todo_id)
        .ok_or(PromoteError::NoTodo)?;
    if todo.done == Some(true) {
        return Err(PromoteError::TodoDone);
    }
    let goal = crate::render::goal_text(&note.body);
    let prompt = match &goal {
        Some(g) => format!(
            "Complete this branch-note todo: {} ({todo_id}).\nBranch goal: {g}",
            todo.text
        ),
        None => format!("Complete this branch-note todo: {} ({todo_id}).", todo.text),
    };
    let key = format!("attn:promote:{}:{}:{todo_id}", ctx.repo, ctx.branch);
    let add = add_new(
        &NewAttn {
            source: "manual",
            title: &todo.text,
            severity,
            repo: Some(&ctx.repo),
            branch: Some(&ctx.branch),
            link: todo.link.as_deref(),
            key: Some(&key),
            mode: Some(mode),
            prompt: Some(&prompt),
            ..Default::default()
        },
        today,
    )
    .map_err(PromoteError::Write)?;
    Ok(PromoteOutcome {
        add,
        ctx,
        todo_text: todo.text.clone(),
    })
}

/// Parse script input for `bn attn ingest`: either a JSON array, a single JSON object, or JSONL
/// (one object per line). Invalid JSONL lines are ignored, matching feed ingest's tolerant shape.
pub fn parse_ingest_items(input: &str) -> Vec<IngestItem> {
    let trimmed = input.trim_start();
    if trimmed.starts_with('[') {
        if let Ok(items) = serde_json::from_str::<Vec<IngestItem>>(trimmed) {
            return items;
        }
    }
    if trimmed.starts_with('{') {
        if let Ok(item) = serde_json::from_str::<IngestItem>(trimmed) {
            return vec![item];
        }
    }
    input
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str::<IngestItem>(l).ok())
        .collect()
}

/// Ingest a batch of script-provided items. `key` values are namespaced by source
/// (`attn:<source>:<key>`) so independent scripts can reuse natural ids without colliding.
/// With `replace`, open keyed items from the affected source(s) that are absent from this batch
/// are acked, letting polling scripts output the current set and keep attention fresh.
pub fn ingest(
    items: &[IngestItem],
    default_source: &str,
    default_severity: &str,
    replace: bool,
    today: &str,
) -> std::io::Result<IngestOutcome> {
    if !SEVERITIES.contains(&default_severity) {
        return Err(invalid_input(format!(
            "invalid severity '{default_severity}' (want {})",
            SEVERITIES.join(" | ")
        )));
    }

    let default_source = non_empty(default_source).unwrap_or("script");
    let mut out = IngestOutcome::default();
    let mut current_keys: std::collections::HashMap<String, std::collections::HashSet<String>> =
        Default::default();
    let mut affected_sources: std::collections::HashSet<String> = Default::default();
    if replace {
        affected_sources.insert(default_source.to_string());
    }

    for item in items {
        let Some(title) = non_empty(&item.title) else {
            continue;
        };
        let source = non_empty_opt(item.source.as_deref()).unwrap_or(default_source);
        let severity = non_empty_opt(item.severity.as_deref()).unwrap_or(default_severity);
        if !SEVERITIES.contains(&severity) {
            return Err(invalid_input(format!(
                "invalid severity '{severity}' for '{title}' (want {})",
                SEVERITIES.join(" | ")
            )));
        }

        affected_sources.insert(source.to_string());
        let key = non_empty_opt(item.key.as_deref()).map(|k| ingest_key(source, k));
        if let Some(k) = &key {
            current_keys
                .entry(source.to_string())
                .or_default()
                .insert(k.clone());
        }

        let add = add_with_detail(
            source,
            title,
            severity,
            non_empty_opt(item.repo.as_deref()),
            non_empty_opt(item.branch.as_deref()),
            non_empty_opt(item.link.as_deref()),
            key.as_deref(),
            today,
            non_empty_opt(item.detail.as_deref()),
        )?;
        if add.skipped {
            out.skipped += 1;
        } else {
            out.raised += 1;
        }
        out.items.push(IngestedItem {
            id: add.id,
            title: title.to_string(),
            source: source.to_string(),
            key,
            skipped: add.skipped,
        });
    }

    if replace {
        for item in open_items(today) {
            if !affected_sources.contains(&item.source) {
                continue;
            }
            let Some(k) = item.key.as_deref() else {
                continue;
            };
            if !k.starts_with(&format!("attn:{}:", item.source)) {
                continue;
            }
            let keep = current_keys
                .get(&item.source)
                .is_some_and(|keys| keys.contains(k));
            if !keep && ack(&item.id)? {
                out.resolved += 1;
            }
        }
    }

    Ok(out)
}

fn ingest_key(source: &str, key: &str) -> String {
    format!("attn:{source}:{key}")
}

fn non_empty(s: &str) -> Option<&str> {
    let s = s.trim();
    (!s.is_empty()).then_some(s)
}

fn non_empty_opt(s: Option<&str>) -> Option<&str> {
    s.and_then(non_empty)
}

fn invalid_input(msg: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attention::*;
    use crate::test_support::{config_with_roots, with_state_dir};
    use std::path::PathBuf;

    fn cfg(root: PathBuf) -> Config {
        config_with_roots(
            root.join("notes"),
            root.join("work-notes"),
            root.join("worktree"),
            "host",
        )
    }

    fn ingest_item(key: &str, title: &str) -> IngestItem {
        IngestItem {
            title: title.into(),
            severity: None,
            source: None,
            key: Some(key.into()),
            link: None,
            repo: None,
            branch: None,
            detail: None,
        }
    }

    #[test]
    fn add_allocates_monotonic_ids() {
        with_state_dir(|| {
            assert_eq!(add_simple("a", "info", None).id, "a1");
            assert_eq!(add_simple("b", "warn", None).id, "a2");
            // ack a1, next id still climbs past it (no reuse)
            ack("a1").unwrap();
            assert_eq!(add_simple("c", "urgent", None).id, "a3");
        });
    }

    #[test]
    fn keyed_completion_readd_is_skipped_not_duplicated() {
        with_state_dir(|| {
            // A completion no longer lives in open_items (#242), but a re-fired `agent done` must
            // still dedup on its key against the review lane — skipped, same id, one item.
            let first = add_simple(
                "done: bn/fix-z — scouted",
                DONE_SEVERITY,
                Some("done:bn/fix-z"),
            );
            assert!(!first.skipped);
            let second = add_simple(
                "done: bn/fix-z — shipped",
                DONE_SEVERITY,
                Some("done:bn/fix-z"),
            );
            assert!(second.skipped, "second keyed done must dedup");
            assert_eq!(second.id, first.id, "dedup returns the existing id");
            assert_eq!(completed_items("2026-06-23").len(), 1);
        });
    }

    #[test]
    fn key_makes_add_idempotent_while_open() {
        with_state_dir(|| {
            let first = add_simple("pr #51 needs review", "warn", Some("pr-queue:bn#51"));
            assert!(!first.skipped);
            let again = add_simple(
                "pr #51 needs review (again)",
                "warn",
                Some("pr-queue:bn#51"),
            );
            assert!(again.skipped);
            assert_eq!(again.id, first.id);
            assert_eq!(open_items("2026-06-23").len(), 1);

            // once acked, the same key may be raised afresh (a new occurrence)
            ack(&first.id).unwrap();
            let third = add_simple("pr #51 needs review", "warn", Some("pr-queue:bn#51"));
            assert!(!third.skipped);
            assert_ne!(third.id, first.id);
        });
    }

    #[test]
    fn keyed_add_dedups_against_started_item_and_refreshes_it() {
        with_state_dir(|| {
            // #534: a re-firing keyed job must dedup even after the item left the open lane. Start
            // the item (moves it into the in-flight set), then re-add the same key with fresh text
            // — it must update the one record in place, not stack a second open duplicate.
            let first = add_simple(
                "merged worktree x/y has 1 uncommitted change(s)",
                "warn",
                Some("wt-stuck:x/y"),
            );
            assert!(!first.skipped);
            mark_started(&first.id).unwrap();
            assert!(
                open_items("2026-06-23").is_empty(),
                "started item left the open lane"
            );

            let again = add_simple(
                "merged worktree x/y has 3 uncommitted change(s)",
                "warn",
                Some("wt-stuck:x/y"),
            );
            assert!(
                again.skipped,
                "keyed re-add must dedup against the started item"
            );
            assert_eq!(again.id, first.id, "dedup returns the existing id");

            // Still exactly one live record, now carrying the refreshed text.
            let started = started_items();
            assert_eq!(started.len(), 1, "no duplicate created");
            assert_eq!(
                started[0].item.title,
                "merged worktree x/y has 3 uncommitted change(s)"
            );
        });
    }

    #[test]
    fn parse_ingest_items_accepts_array_single_object_and_jsonl_aliases() {
        let arr = parse_ingest_items(
            r#"[{"text":"Reply to mail","url":"https://mail","key":"m1","severity":"urgent"}]"#,
        );
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].title, "Reply to mail");
        assert_eq!(arr[0].link.as_deref(), Some("https://mail"));

        let one = parse_ingest_items(r#"{"title":"One item","key":"one"}"#);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].title, "One item");

        let lines = parse_ingest_items(
            r#"
{"title":"A","key":"a"}
not json
{"text":"B","key":"b","source":"job"}
"#,
        );
        let titles: Vec<_> = lines.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, vec!["A", "B"]);
        assert_eq!(lines[1].source.as_deref(), Some("job"));
    }

    #[test]
    fn ingest_uses_source_namespaced_keys_for_idempotent_script_adds() {
        with_state_dir(|| {
            let batch = vec![ingest_item("m1", "Reply to mail")];
            let first = ingest(&batch, "email", "warn", false, "2026-06-23").unwrap();
            assert_eq!(first.raised, 1);
            assert_eq!(first.skipped, 0);
            assert_eq!(first.items[0].key.as_deref(), Some("attn:email:m1"));

            let again = ingest(&batch, "email", "warn", false, "2026-06-23").unwrap();
            assert_eq!(again.raised, 0);
            assert_eq!(again.skipped, 1);
            assert_eq!(open_items("2026-06-23").len(), 1);
        });
    }

    #[test]
    fn ingest_replace_acks_keyed_items_missing_from_batch() {
        with_state_dir(|| {
            let first = vec![
                ingest_item("m1", "Reply to mail"),
                ingest_item("m2", "Ping"),
            ];
            ingest(&first, "email", "warn", true, "2026-06-23").unwrap();
            assert_eq!(open_items("2026-06-23").len(), 2);

            let second = vec![ingest_item("m2", "Ping")];
            let out = ingest(&second, "email", "warn", true, "2026-06-23").unwrap();
            assert_eq!(out.resolved, 1);
            let open = open_items("2026-06-23");
            assert_eq!(open.len(), 1);
            assert_eq!(open[0].title, "Ping");

            let out = ingest(&[], "email", "warn", true, "2026-06-23").unwrap();
            assert_eq!(out.resolved, 1, "empty replace clears the source");
            assert!(open_items("2026-06-23").is_empty());
        });
    }

    #[test]
    fn plan_item_creates_todo_on_target_note_and_resolves_attention() {
        with_state_dir(|| {
            let root = tempfile::tempdir().unwrap();
            let cfg = cfg(root.path().to_path_buf());
            let it = add(
                "manual",
                "follow up",
                "warn",
                Some("MyRepo"),
                Some("feature/x"),
                Some("https://example.test/task"),
                None,
                "2026-06-23",
            )
            .unwrap();

            let out = plan_item(&cfg, Path::new("/nowhere"), &it.id, "todo", "2026-06-23")
                .expect("plan attention item");

            assert_eq!(out.ctx.repo, "myrepo");
            assert_eq!(out.ctx.branch, "feature-x");
            assert_eq!(out.todo.text, "follow up — https://example.test/task");
            assert!(open_items("2026-06-23").is_empty());

            let note = crate::store::read(&cfg.note_file(&out.ctx)).unwrap();
            assert_eq!(note.front.todos.len(), 1);
            assert_eq!(note.front.todos[0].id, out.todo.id);
        });
    }

    #[test]
    fn add_new_writes_mode_and_prompt() {
        with_state_dir(|| {
            let out = add_new(
                &NewAttn {
                    source: "manual",
                    title: "refactor resolver",
                    severity: "warn",
                    repo: Some("bn"),
                    branch: Some("resolver-cleanup"),
                    mode: Some("auto"),
                    prompt: Some("Refactor the resolver; run tests."),
                    ..Default::default()
                },
                "2026-06-23",
            )
            .unwrap();
            let item = get(&out.id).unwrap();
            assert_eq!(item.mode(), "auto");
            assert_eq!(item.mode.as_deref(), Some("auto"));
            assert_eq!(
                item.prompt.as_deref(),
                Some("Refactor the resolver; run tests.")
            );
            // A plain item defaults to manual, no prompt.
            let plain = add_simple("plain", "info", None);
            let p = get(&plain.id).unwrap();
            assert_eq!(p.mode(), "manual");
            assert!(p.mode.is_none() && p.prompt.is_none());
        });
    }

    #[test]
    fn add_new_writes_and_refreshes_structured_worktree_fields() {
        with_state_dir(|| {
            let files = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
            let out = add_new(
                &NewAttn {
                    source: "wt-cleanup",
                    title: "merged worktree bn/feat has 2 uncommitted change(s)",
                    severity: "warn",
                    repo: Some("bn"),
                    branch: Some("feat"),
                    key: Some("wt-stuck:bn/feat"),
                    worktree_path: Some("/wt/bn/feat"),
                    uncommitted_files: files.clone(),
                    ..Default::default()
                },
                "2026-06-23",
            )
            .unwrap();
            let item = get(&out.id).unwrap();
            assert_eq!(item.worktree_path.as_deref(), Some("/wt/bn/feat"));
            assert_eq!(item.uncommitted_files, files);

            // A re-fire whose dirty set shrank refreshes the keyed record in place — same id, new
            // title count *and* new file list overlaid.
            let refire = add_new(
                &NewAttn {
                    source: "wt-cleanup",
                    title: "merged worktree bn/feat has 1 uncommitted change(s)",
                    severity: "warn",
                    repo: Some("bn"),
                    branch: Some("feat"),
                    key: Some("wt-stuck:bn/feat"),
                    worktree_path: Some("/wt/bn/feat"),
                    uncommitted_files: vec!["src/a.rs".to_string()],
                    ..Default::default()
                },
                "2026-06-23",
            )
            .unwrap();
            assert_eq!(refire.id, out.id);
            assert!(refire.skipped);
            let refreshed = get(&out.id).unwrap();
            assert_eq!(refreshed.uncommitted_files, vec!["src/a.rs".to_string()]);
        });
    }

    #[test]
    fn folds_legacy_attn_event_without_structured_fields() {
        with_state_dir(|| {
            // An item raised before the fields existed carries neither — folds to the empty
            // defaults, never a panic or a stale value.
            let out = add_simple("legacy notice", "warn", None);
            let item = get(&out.id).unwrap();
            assert!(item.worktree_path.is_none());
            assert!(item.uncommitted_files.is_empty());
        });
    }

    /// Write a note for bn/feat-x with a goal and todos (t1 open with a link, t2 done).
    fn write_note_with_todos(cfg: &Config) -> Resolved {
        let ctx = Resolved {
            repo: "bn".into(),
            branch: "feat-x".into(),
        };
        let path = cfg.note_file(&ctx);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "---\nrepo: bn\nbranch: feat-x\ncreated: 2026-06-20\nstatus: active\nmachine: host\n\
             tags: []\ncollaborators: []\ntodos:\n- id: t1\n  text: add retry logic\n  created: 2026-06-20\n  \
             link: https://issue/9\n- id: t2\n  text: already finished\n  created: 2026-06-20\n  done: true\n\
             blockers: []\ndecisions: []\nresearch: []\ncollab: []\nask: []\n---\n\
             # bn / feat-x\n\n## Goal\n\nship the retry feature\n",
        )
        .unwrap();
        ctx
    }

    #[test]
    fn promote_todo_seeds_an_auto_item_from_the_note() {
        with_state_dir(|| {
            let root = tempfile::tempdir().unwrap();
            let cfg = cfg(root.path().to_path_buf());
            write_note_with_todos(&cfg);

            let out = promote_todo(
                &cfg,
                Path::new("/nowhere"),
                Some("bn".into()),
                Some("feat-x".into()),
                "t1",
                "auto",
                "warn",
                "2026-06-23",
            )
            .expect("promote t1");
            assert!(!out.add.skipped);
            assert_eq!(out.todo_text, "add retry logic");

            let item = get(&out.add.id).unwrap();
            assert_eq!(item.mode(), "auto");
            assert_eq!(item.title, "add retry logic");
            assert_eq!(item.repo.as_deref(), Some("bn"));
            assert_eq!(item.branch.as_deref(), Some("feat-x"));
            assert_eq!(item.link.as_deref(), Some("https://issue/9"));
            let prompt = item.prompt.unwrap();
            assert!(prompt.contains("add retry logic"), "{prompt}");
            assert!(
                prompt.contains("ship the retry feature"),
                "goal seeds the prompt: {prompt}"
            );
            // eligible for the loop right away
            assert_eq!(next_auto("2026-06-23", 1).map(|i| i.id), Some(out.add.id));
        });
    }

    #[test]
    fn promote_todo_is_idempotent_and_rejects_done_or_unknown() {
        with_state_dir(|| {
            let root = tempfile::tempdir().unwrap();
            let cfg = cfg(root.path().to_path_buf());
            write_note_with_todos(&cfg);
            let target = |id: &str| {
                promote_todo(
                    &cfg,
                    Path::new("/nowhere"),
                    Some("bn".into()),
                    Some("feat-x".into()),
                    id,
                    "auto",
                    "warn",
                    "2026-06-23",
                )
            };
            let first = target("t1").expect("first promote");
            let again = target("t1").expect("re-promote");
            assert!(again.add.skipped, "same todo → keyed no-op");
            assert_eq!(again.add.id, first.add.id);

            assert!(matches!(target("t2"), Err(PromoteError::TodoDone)));
            assert!(matches!(target("t9"), Err(PromoteError::NoTodo)));
        });
    }

    #[test]
    fn task_type_folds_round_trip() {
        with_state_dir(|| {
            let out = add_new(
                &NewAttn {
                    source: "manual",
                    title: "critic findings",
                    severity: "warn",
                    task_type: Some("critique"),
                    ..Default::default()
                },
                "2026-06-23",
            )
            .unwrap();
            let item = get(&out.id).expect("raised item folds back");
            assert_eq!(item.task_type.as_deref(), Some("critique"));
            assert_eq!(item.task_type(), "critique");
        });
    }

    #[test]
    fn absent_task_type_defaults_to_coding() {
        with_state_dir(|| {
            // A plain (untyped) item — the common case and every legacy pre-#649 item — folds with
            // no `task_type` field yet reads back as the `coding` execution stage.
            let out = add_simple("plain work", "warn", None);
            let item = get(&out.id).expect("item folds back");
            assert!(
                item.task_type.is_none(),
                "no field written for untyped item"
            );
            assert_eq!(item.task_type(), "coding");
            // …and the absent field is skipped on serialize, keeping the payload byte-identical.
            let v = serde_json::to_value(&item).unwrap();
            assert!(v.get("task_type").is_none());
        });
    }
}
