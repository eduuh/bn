//! `bn task` — **the** task list (#996), one global store for every repo and every machine.
//!
//! The file is `<personal_root>/tasks.txt` (`Config::tasks_file`), in todo.txt grammar. It used to
//! be a per-repo `TODO.txt` committed into each working tree, which made the list per-repo *and*
//! per-branch — the opposite of a single source of truth. Consequences of going global:
//!
//! - **No git repo is required.** `bn task` works from anywhere; repo/branch context, when there
//!   is any, only supplies defaults for `repo:`/`branch:` on add and resolves `--repo .`.
//! - **The bare view is "what could I do today"**, not the whole backlog: `day:` today-or-earlier
//!   plus unscheduled work. `--all` is the backlog.
//! - A leftover repo-local `TODO.txt` earns a one-line deprecation notice and `bn task import`.
use crate::*;

/// A mutation on a single task, dispatched by [`emit_task_mutate`].
pub(crate) enum TaskOp<'a> {
    Do,
    Wip,
    Snooze(&'a str),
    Cancel(Option<&'a str>),
}

/// Which slice of the store a listing shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskView {
    /// The default: work that is actionable *now* — `day:` today or earlier (so an overdue task
    /// stays visible rather than silently ageing out), plus everything unscheduled.
    Today,
    /// Every open task, whatever it is scheduled for.
    All,
}

/// The repo scope a listing is filtered to.
pub(crate) enum RepoScope {
    /// No filter — every repo.
    Any,
    /// A specific repo name (already lowercased).
    Named(String),
}

/// Resolve `--repo`: `.` means the repo you are standing in, anything else is a name. Exits with
/// guidance when `.` is used outside a git repo, since there is nothing to resolve it to.
pub(crate) fn repo_scope(cfg: &Config, repo: Option<&str>) -> RepoScope {
    match repo {
        None => RepoScope::Any,
        Some(".") => match task_ctx_opt(cfg) {
            Some(ctx) => RepoScope::Named(ctx.repo.to_ascii_lowercase()),
            None => fail("bn task --repo .: not in a git repo — pass a repo name instead"),
        },
        Some(name) => RepoScope::Named(name.trim().to_ascii_lowercase()),
    }
}

impl RepoScope {
    fn admits(&self, task: &bn_core::todo::Task) -> bool {
        match self {
            RepoScope::Any => true,
            RepoScope::Named(want) => task
                .meta("repo")
                .is_some_and(|r| r.eq_ignore_ascii_case(want)),
        }
    }
}

/// The global task store's path. No repo required — that is the point of the store.
pub(crate) fn task_path(cfg: &Config) -> std::path::PathBuf {
    cfg.tasks_file()
}

/// The current worktree's repo/branch, when there is one. `None` outside a git repo, which is a
/// normal way to run `bn task` now rather than an error.
pub(crate) fn task_ctx_opt(cfg: &Config) -> Option<Resolved> {
    target_ctx(cfg, None, None)
}

/// `bn task claim <id> [--repo] [--branch]` — bind a task to the worktree working it.
///
/// The write that makes the fleet's "what is this agent working on?" answerable: it stamps the
/// `repo:`/`branch:` metas the `(repo, branch)` join already keys on. `bn dispatch --task` does
/// this automatically; this is the hand path — an agent running it from inside its own checkout,
/// or an operator adopting a worktree that was started before the task existed.
pub(crate) fn emit_task_claim(
    cfg: &Config,
    id: &str,
    repo: Option<String>,
    branch: Option<String>,
    as_json: bool,
) {
    let (repo, branch) = match (repo, branch) {
        (Some(r), Some(b)) => (r, b),
        (r, b) => match task_ctx_opt(cfg) {
            // A partial `--repo`/`--branch` is completed from the cwd rather than refused.
            Some(ctx) => (r.unwrap_or(ctx.repo), b.unwrap_or(ctx.branch)),
            None => fail_json(
                as_json,
                "task_claim",
                "no_target",
                "bn task claim: not in a git repo — pass --repo and --branch",
            ),
        },
    };
    let path = task_path(cfg);
    match bn_core::todo::claim(&path, id, &repo, &branch) {
        Ok(true) => {}
        Ok(false) => fail_json(
            as_json,
            "task_claim",
            "not_found",
            &format!("bn task claim: no task '{id}'"),
        ),
        Err(e) => fail_json(
            as_json,
            "task_claim",
            "write_failed",
            &format!("bn task claim: {e}"),
        ),
    }
    let data = serde_json::json!({ "id": id, "repo": repo.to_lowercase(), "branch": branch });
    if as_json {
        println!("{}", Envelope::success("task_claim", data).to_json());
    } else {
        println!("{id} → {}/{branch}", repo.to_lowercase());
    }
}

/// Where a task sits relative to `today`, for the bare view's ordering.
///
/// `None` means "not actionable today" — scheduled for a future day, so it folds out of the
/// default listing the way a snoozed task does. A malformed `day:` is treated as unscheduled
/// rather than as a future date: the same discipline as `snooze:`, so a hand-edit or a merge
/// artifact can never hide work forever.
fn day_bucket(task: &bn_core::todo::Task, today: &str) -> Option<u8> {
    match task
        .meta("day")
        .filter(|d| bn_core::util::epoch_day(d).is_some())
    {
        Some(day) if day > today => None,
        Some(_) => Some(0),
        None => Some(1),
    }
}

/// Warn once when a repo-local `TODO.txt` still holds work the global store has not absorbed.
///
/// Best-effort and non-blocking: on stderr so it never pollutes `--json`, and skipped entirely
/// outside a git repo or when the legacy file is missing or empty.
fn warn_legacy_todo() {
    let cwd = std::env::current_dir().unwrap_or_default();
    let Some(root) = bn_core::resolver::repo_root(&cwd) else {
        return;
    };
    let legacy = root.join("TODO.txt");
    let has_tasks = std::fs::read_to_string(&legacy)
        .map(|s| s.lines().any(|l| !l.trim().is_empty()))
        .unwrap_or(false);
    if has_tasks {
        eprintln!(
            "note: {} is deprecated — tasks now live in one global store. Import it with: bn task import {}",
            legacy.display(),
            legacy.display()
        );
    }
}

/// The rows a listing shows, paired with their day bucket, most-actionable first.
///
/// Pure, so the view semantics are testable without a store or a terminal. Ordering is the day
/// bucket first (today/overdue, then unscheduled, then — in `--all` — future work), and within a
/// bucket the sort is stable, so `open_sorted`'s priority → due → id ranking is preserved.
fn select_tasks<'a>(
    file: &'a bn_core::todo::TodoFile,
    view: TaskView,
    scope: &RepoScope,
    today: &str,
) -> Vec<(u8, &'a bn_core::todo::Task)> {
    let mut items: Vec<(u8, &bn_core::todo::Task)> = file
        .open_sorted(today, None)
        .into_iter()
        .filter(|t| scope.admits(t))
        .filter_map(|t| match (view, day_bucket(t, today)) {
            // `--all` keeps future-dated work, sorted after everything actionable.
            (TaskView::All, bucket) => Some((bucket.unwrap_or(2), t)),
            (TaskView::Today, bucket) => bucket.map(|b| (b, t)),
        })
        .collect();
    items.sort_by_key(|(bucket, _)| *bucket);
    items
}

pub(crate) fn emit_task_list(cfg: &Config, view: TaskView, repo: Option<&str>, as_json: bool) {
    warn_legacy_todo();
    let scope = repo_scope(cfg, repo);
    let path = task_path(cfg);
    let file = bn_core::todo::load(&path).unwrap_or_else(|e| fail(&format!("bn task: {e}")));
    let today = today();

    let items = select_tasks(&file, view, &scope, &today);

    if as_json
        && json_ok(
            "task",
            serde_json::json!({
                "view": match view { TaskView::Today => "today", TaskView::All => "all" },
                "repo": match &scope { RepoScope::Any => None, RepoScope::Named(r) => Some(r.clone()) },
                "today": today,
                "tasks": items.iter().map(|(_, t)| t.to_json(&today)).collect::<Vec<_>>(),
            }),
        )
    {
        return;
    }
    if items.is_empty() {
        let where_ = match &scope {
            RepoScope::Any => String::new(),
            RepoScope::Named(r) => format!(" in {r}"),
        };
        match view {
            TaskView::Today => println!(
                "Nothing scheduled for today{where_} — `bn task --all` for the backlog, `bn task add` to start one."
            ),
            TaskView::All => println!("No open tasks{where_}."),
        }
        return;
    }
    for (bucket, t) in &items {
        let pri = t.priority.map(|c| format!("({c}) ")).unwrap_or_default();
        let due = t
            .meta("due")
            .map(|d| format!("  due:{d}"))
            .unwrap_or_default();
        // The store spans repos now, so *where* a task lives is part of every row, not a
        // `--all`-only extra the way `[branch]` used to be.
        let repo = t
            .meta("repo")
            .map(|r| format!("  [{r}]"))
            .unwrap_or_default();
        // Mark work carried over from an earlier day, so an overdue task doesn't read as fresh.
        let day = match t.meta("day") {
            Some(d) if *bucket == 0 && d < today.as_str() => format!("  day:{d}!"),
            Some(d) if *bucket == 0 => format!("  day:{d}"),
            Some(d) => format!("  day:{d}"),
            None => String::new(),
        };
        println!(
            "{}  {}{}{}{}{}",
            t.id().unwrap_or("?"),
            pri,
            t.summary(),
            due,
            day,
            repo
        );
    }
}

pub(crate) fn emit_task_add(
    cfg: &Config,
    text: &str,
    priority: Option<char>,
    due: Option<&str>,
    src: &str,
    capture: &crate::cmd::task_capture::Capture,
    as_json: bool,
) {
    if text.trim().is_empty() {
        fail("bn task add: text must not be empty");
    }
    let priority = priority.map(|c| c.to_ascii_uppercase());
    if let Some(p) = priority {
        if !('A'..='C').contains(&p) {
            fail("bn task add: -p/--priority must be A, B, or C");
        }
    }
    if let Some(d) = due {
        if bn_core::util::epoch_day(d).is_none() {
            fail("bn task add: --due must be a date (YYYY-MM-DD)");
        }
    }
    warn_legacy_todo();
    let path = task_path(cfg);
    let ctx = task_ctx_opt(cfg);
    // Serialize this read-modify-write against a concurrent mutate/ingest rewrite (held to fn end).
    let _lock = bn_core::todo::lock(&path);
    // Append-only: read just to mint the next id, then append one line — a concurrent add
    // concatenates rather than clobbering, and existing lines are never reflowed.
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let mut file = bn_core::todo::load(&path).unwrap_or_else(|e| fail(&format!("bn task: {e}")));
    let today = today();
    let id = file.next_id();
    let branch = ctx.as_ref().map(|c| c.branch.as_str()).unwrap_or("");
    let mut task = bn_core::todo::Task::new_todo(&id, text, priority, branch, src, &today);
    if let Some(d) = due {
        task.set_meta("due", d);
    }
    // Default `repo:` from where you're standing, unless the text already named one inline
    // (`bn task add "… repo:atlas"` classifies straight into a meta token). The store spans repos,
    // so a task with no repo is one nothing can route — #998 makes that a hard gate.
    if task.meta("repo").is_none() {
        if let Some(repo) = ctx.as_ref().map(|c| c.repo.to_ascii_lowercase()) {
            task.set_meta("repo", &repo);
        }
    }
    let is_new = !path.exists();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Read back before the task is handed to the file below — the append and the save paths both
    // consume it, and the JSON envelope reports where it landed.
    let stamped_repo = task.meta("repo").map(str::to_string);
    // Kept for the capture pass below: `task` itself is consumed by whichever persist path runs.
    let captured_task = task.clone();
    if raw == file.render() {
        // File is already normalized (unique ids, canonical form) — keep the concurrency-safe
        // append: a parallel add concatenates instead of clobbering, and no line is reflowed.
        bn_core::todo::append_task(&path, &task).unwrap_or_else(|e| fail(&format!("bn task: {e}")));
    } else {
        // `load` had to heal the file (duplicate/missing ids, e.g. after a union merge). Persist
        // the healed numbering now, with the new task, so the ids the user just saw in `list` stay
        // stable instead of being re-derived — and shifted — on the next read.
        file.push_task(task);
        bn_core::todo::save(&path, &file).unwrap_or_else(|e| fail(&format!("bn task: {e}")));
    }
    if is_new {
        // Provision a union merge driver so two machines' appends concatenate instead of
        // conflicting; `load` repairs the concatenation (#995).
        if let Some(root) = path.parent() {
            let _ = bn_core::todo::ensure_union_merge(root);
        }
    }
    // Only once the line is durable: a capture that fails must cost context, never the task.
    let (stored, problems) = if capture.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        crate::cmd::task_capture::record(cfg, &captured_task, capture)
    };
    if as_json {
        let data = serde_json::json!({
            "id": id,
            "repo": stamped_repo,
            "branch": branch,
            "attachments": stored,
            "warnings": problems,
        });
        println!("{}", Envelope::success("task_add", data).to_json());
    } else {
        println!("Added {id}");
        crate::cmd::task_capture::report(&stored, &problems);
    }
}

/// `bn task import <path>` — absorb a legacy repo-local `TODO.txt` into the global store.
///
/// Idempotent by construction: each imported line carries `key:import:<hash>` derived from its
/// original text, and a line whose key is already present is skipped. So re-running after adding
/// more tasks to the legacy file imports only what is new, and re-running unchanged imports
/// nothing.
///
/// Ids are reminted against the global store (the legacy numbering is repo-local and would
/// collide), each line gains a `uid:` so it is dedupable from here on, and `repo:` is filled from
/// `--repo` or the source file's repo directory. Existing `branch:` tags are preserved.
pub(crate) fn emit_task_import(
    cfg: &Config,
    source: &std::path::Path,
    repo: Option<&str>,
    as_json: bool,
) {
    let legacy = bn_core::todo::load(source)
        .unwrap_or_else(|e| fail(&format!("bn task import: cannot read {}: {e}", source.display())));

    // `--repo` wins; otherwise name the repo after the directory the legacy file sits in, which is
    // the repo root for the `<repo>/TODO.txt` layout this imports from.
    let repo = repo
        .map(|r| r.trim().to_ascii_lowercase())
        .or_else(|| {
            source
                .canonicalize()
                .ok()?
                .parent()?
                .file_name()?
                .to_str()
                .map(|s| s.to_ascii_lowercase())
        })
        .unwrap_or_default();

    let path = task_path(cfg);
    let _lock = bn_core::todo::lock(&path);
    let is_new = !path.exists();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut store = bn_core::todo::load(&path).unwrap_or_else(|e| fail(&format!("bn task: {e}")));

    let seen: std::collections::HashSet<String> = store
        .tasks()
        .filter_map(|t| t.meta("key").map(str::to_string))
        .collect();

    let (mut added, mut skipped) = (0usize, 0usize);
    let mut ids = Vec::new();
    for task in legacy.tasks() {
        let key = import_key(task);
        if seen.contains(&key) {
            skipped += 1;
            continue;
        }
        let mut task = task.clone();
        let id = store.next_id();
        task.set_meta_raw("id", &id);
        task.set_meta_raw("key", &key);
        if task.meta("repo").is_none() && !repo.is_empty() {
            task.set_meta_raw("repo", &repo);
        }
        task.ensure_uid();
        task.touch();
        store.push_task(task);
        ids.push(id);
        added += 1;
    }

    if added > 0 {
        bn_core::todo::save(&path, &store).unwrap_or_else(|e| fail(&format!("bn task: {e}")));
        if is_new {
            if let Some(root) = path.parent() {
                let _ = bn_core::todo::ensure_union_merge(root);
            }
        }
    }

    if as_json {
        let data = serde_json::json!({ "added": added, "skipped": skipped, "ids": ids, "repo": repo });
        println!("{}", Envelope::success("task_import", data).to_json());
    } else {
        println!(
            "Imported {added} task{} from {} ({skipped} already present)",
            if added == 1 { "" } else { "s" },
            source.display()
        );
        if added > 0 {
            println!("The original file is left in place — delete it once you're happy.");
        }
    }
}

/// `bn task doc <id>` — open (creating on first use) a task's context doc.
///
/// The doc is where everything the one-line task can't hold lives: why it matters, the
/// `## Done When` checklist the SMART gate reads (#998), investigation findings, open questions.
/// Its location is resolved by `task_doc::doc_path` — beside the branch note once the task has
/// been picked up, in staging before that — so the caller never has to know which.
pub(crate) fn emit_task_doc(cfg: &Config, id: &str, open: bool, as_json: bool) {
    let path = task_path(cfg);
    let _lock = bn_core::todo::lock(&path);
    let mut file = bn_core::todo::load(&path).unwrap_or_else(|e| fail(&format!("bn task: {e}")));
    let Some(task) = file.task_mut(id) else {
        fail(&format!("bn task doc: no task {id}"));
    };

    let existed = bn_core::task_doc::doc_path(cfg, task).is_some_and(|p| p.exists());
    let doc = bn_core::task_doc::ensure(cfg, task)
        .unwrap_or_else(|e| fail(&format!("bn task doc: {e}")));

    // Record that this task has a doc, so `bn task --json` and the SMART gate can see it without
    // a filesystem probe. Stored as a bare file name — never a path, which could point at the
    // wrong directory once the doc is copied beside the branch note.
    let name = doc
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let already = task.meta(bn_core::task_doc::DOC_KEY) == Some(name.as_str());
    if !already && !bn_core::task_doc::is_note_copy(&doc) {
        task.set_meta(bn_core::task_doc::DOC_KEY, &name);
        bn_core::todo::save(&path, &file).unwrap_or_else(|e| fail(&format!("bn task: {e}")));
    }
    drop(_lock);

    if as_json {
        let data = serde_json::json!({ "id": id, "doc": doc.display().to_string(), "created": !existed });
        println!("{}", Envelope::success("task_doc", data).to_json());
        return;
    }
    if !existed {
        println!("Created {}", doc.display());
    }
    if open {
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
        let status = std::process::Command::new(&editor).arg(&doc).status();
        if status.is_err() {
            fail(&format!("bn task doc: could not launch {editor}"));
        }
    } else {
        println!("{}", doc.display());
    }
}

/// `bn task pull` — top the task list back up from the GitHub-issue reservoir.
///
/// Bounded by a low-water mark per repo, so the store stays a curated working set rather than a
/// mirror of every open issue. Ranking, exclusions and the unassigned filter all come from
/// `orchestrate::plan_keep_warm` — this is not a second replenisher, it is the same policy
/// realized as tasks. Idempotent: an issue already carrying a task is never minted again.
///
/// Reads the #999 issue cache, so a stale cache still yields a pull (reported on stdout) rather
/// than refusing to work because `gh` is down.
pub(crate) fn emit_task_pull(
    cfg: &Config,
    repo: Option<&str>,
    low_water: usize,
    dry_run: bool,
    as_json: bool,
) {
    let scope = repo_scope(cfg, repo);
    let repos: Vec<String> = match &scope {
        RepoScope::Named(r) => vec![r.clone()],
        RepoScope::Any => bn_core::issues::known_repos(cfg),
    };
    if repos.is_empty() {
        fail("bn task pull: no repos known — set [managed_repos].repos or create a branch note");
    }

    let policy = bn_core::orchestrate::KeepWarmPolicy {
        low_water,
        ..Default::default()
    };
    let now = bn_core::util::now_epoch_secs();
    let today = today();
    let runner = bn_core::runner::RealRunner;

    let path = task_path(cfg);
    let _lock = bn_core::todo::lock(&path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut file = bn_core::todo::load(&path).unwrap_or_else(|e| fail(&format!("bn task: {e}")));

    let mut minted: Vec<serde_json::Value> = Vec::new();
    let mut stale_repos: Vec<String> = Vec::new();
    let mut unreachable: Vec<(String, String)> = Vec::new();

    for name in &repos {
        let cached = bn_core::issues::for_repo(cfg, &runner, name, now);
        if let Some(err) = &cached.error {
            if cached.stale {
                stale_repos.push(name.clone());
            } else {
                unreachable.push((name.clone(), err.clone()));
                continue;
            }
        }
        // Recomputed per repo: each mint changes both the depth and the dedupe set, so hoisting
        // these out of the loop would let one repo's pull mis-size the next one's.
        let existing = bn_core::task_pull::existing_srcs(&file);
        let depth = bn_core::task_pull::active_depth(&file, name, &today);
        let chosen =
            bn_core::task_pull::plan_pull(name, &policy, depth, &existing, &cached.issues, now);

        for issue in chosen {
            let id = file.next_id();
            let task = bn_core::task_pull::task_from_issue(&id, &issue, &today);
            minted.push(serde_json::json!({
                "id": id, "repo": name, "issue": issue.number,
                "url": issue.url, "title": issue.title,
            }));
            // Push even on a dry run, so the preview shows the ids and per-repo depths a real run
            // would actually produce. Only the `save` below is skipped.
            file.push_task(task);
        }
    }

    if !dry_run && !minted.is_empty() {
        bn_core::todo::save(&path, &file).unwrap_or_else(|e| fail(&format!("bn task: {e}")));
    }
    drop(_lock);

    if as_json {
        let data = serde_json::json!({
            "dry_run": dry_run,
            "low_water": low_water,
            "pulled": minted.len(),
            "tasks": minted,
            "stale_repos": stale_repos,
            "unreachable": unreachable.iter().map(|(r, e)| serde_json::json!({"repo": r, "reason": e})).collect::<Vec<_>>(),
        });
        println!("{}", Envelope::success("task_pull", data).to_json());
        return;
    }

    // Never let a silent skip read as "nothing to pull" — the whole point of #999's honesty.
    for (name, why) in &unreachable {
        eprintln!("note: {name} could not be reached and has no cache — {why}");
    }
    if !stale_repos.is_empty() {
        eprintln!(
            "note: served stale issue data for {} (gh unavailable)",
            stale_repos.join(", ")
        );
    }
    if minted.is_empty() {
        // "Nothing to pull" and "we could not ask" are different facts, and conflating them is
        // precisely the failure #999 exists to remove — don't reintroduce it one layer up.
        if unreachable.len() == repos.len() {
            println!(
                "Could not reach {} — no issue data, so nothing was pulled.",
                if repos.len() == 1 {
                    repos[0].clone()
                } else {
                    format!("{} repos", repos.len())
                }
            );
        } else if !unreachable.is_empty() {
            println!(
                "Nothing to pull from the {} repo(s) reachable — {} could not be reached.",
                repos.len() - unreachable.len(),
                unreachable.len()
            );
        } else {
            println!("Nothing to pull — every repo is at its low-water mark of {low_water}.");
        }
        return;
    }
    let verb = if dry_run { "Would pull" } else { "Pulled" };
    println!("{verb} {} task(s):", minted.len());
    for m in &minted {
        println!(
            "  {}  {}  [{}] {}",
            m["id"].as_str().unwrap_or("?"),
            m["title"].as_str().unwrap_or(""),
            m["repo"].as_str().unwrap_or(""),
            m["url"].as_str().unwrap_or("")
        );
    }
    if !dry_run {
        println!("None are executable yet — they need `bn task doc` + a done-when before the gate admits them.");
    }
}

/// A stable idempotency key for an imported line: `import:<hash of its content>`.
///
/// Hashes the *summary* rather than the rendered line, so re-importing still skips a task whose
/// repo-local id or bookkeeping meta shifted between runs — the words are what identify it.
fn import_key(task: &bn_core::todo::Task) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    task.summary().hash(&mut h);
    format!("import:{:016x}", h.finish())
}

pub(crate) fn emit_task_mutate(cfg: &Config, op: TaskOp, id: &str, as_json: bool) {
    if let TaskOp::Snooze(until) = op {
        if bn_core::util::epoch_day(until).is_none() {
            fail("bn task snooze: date must be YYYY-MM-DD");
        }
    }
    warn_legacy_todo();
    let path = task_path(cfg);
    // Serialize the load→mutate→save against a concurrent append add (held to fn end).
    let _lock = bn_core::todo::lock(&path);
    let mut file = bn_core::todo::load(&path).unwrap_or_else(|e| fail(&format!("bn task: {e}")));
    let today = today();
    let (ok, verb) = match op {
        TaskOp::Do => (file.mark_done(id, &today), "Done"),
        TaskOp::Wip => (file.mark_wip(id), "In progress:"),
        TaskOp::Snooze(until) => (file.snooze(id, until), "Snoozed"),
        TaskOp::Cancel(reason) => (file.cancel(id, &today, reason), "Cancelled"),
    };
    if !ok {
        fail(&format!("bn task: no task {id}"));
    }

    // Marking a task `@wip` is the moment it is picked up, so this is where its context doc moves
    // in beside the branch note (#997) — a copy, so the staging path never dangles. Best-effort:
    // a task with no branch yet, or no doc, simply has nothing to adopt.
    let adopted = match op {
        TaskOp::Wip => file
            .tasks()
            .find(|t| t.id() == Some(id))
            .and_then(|t| bn_core::task_doc::adopt_into_note(cfg, t).ok().flatten()),
        _ => None,
    };

    bn_core::todo::save(&path, &file).unwrap_or_else(|e| fail(&format!("bn task: {e}")));
    if as_json {
        let data = serde_json::json!({
            "id": id,
            "ok": true,
            "doc": adopted.as_ref().map(|p| p.display().to_string()),
        });
        println!("{}", Envelope::success("task", data).to_json());
    } else {
        println!("{verb} {id}");
        if let Some(doc) = adopted {
            println!("  context doc: {}", doc.display());
        }
    }
}

pub(crate) fn emit_task_ingest(
    cfg: &Config,
    source: &str,
    branch: Option<&str>,
    replace: bool,
    as_json: bool,
) {
    use std::io::Read;
    let path = task_path(cfg);
    let ctx = task_ctx_opt(cfg);
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        fail("bn task ingest: failed to read stdin");
    }
    let items = bn_core::todo::parse_ingest_todos(&input);
    let default_branch = branch
        .or(ctx.as_ref().map(|c| c.branch.as_str()))
        .unwrap_or("");
    // Serialize the load→ingest→save against a concurrent append add (held to fn end).
    let _lock = bn_core::todo::lock(&path);
    let is_new = !path.exists();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut file = bn_core::todo::load(&path).unwrap_or_else(|e| fail(&format!("bn task: {e}")));
    let out = file
        .ingest(&items, source, default_branch, replace, &today())
        .unwrap_or_else(|e| fail(&format!("bn task ingest: {e}")));
    bn_core::todo::save(&path, &file).unwrap_or_else(|e| fail(&format!("bn task: {e}")));
    if is_new {
        if let Some(root) = path.parent() {
            let _ = bn_core::todo::ensure_union_merge(root);
        }
    }
    if as_json {
        let data = serde_json::json!({
            "added": out.added, "skipped": out.skipped, "resolved": out.resolved, "ids": out.ids,
        });
        println!("{}", Envelope::success("task_ingest", data).to_json());
    } else {
        println!(
            "todo ingest '{}': +{} added, {} skipped, {} resolved",
            source, out.added, out.skipped, out.resolved
        );
    }
}

// --- The two task agents (#1002, EPIC #993) -----------------------------------------------

/// Whether this machine is the one allowed to rewrite the shared task store wholesale.
///
/// Triage restamps `day:` across every open task, so two laptops running it on the same synced
/// file would fight — the union-merge repair in #995 collapses duplicates, but the *decisions*
/// would still flip-flop. `[staff].home` is the shipped "only one machine does this" knob, so
/// reuse it rather than inventing a second one.
fn triage_allowed_here(cfg: &Config) -> bool {
    match cfg.staff_home.as_deref() {
        Some(home) => home == cfg.machine(),
        None => true,
    }
}

struct TaskAgentRun {
    prompt: String,
    launched: bool,
    output: String,
}

/// Strictly render a task agent and run its configured headless harness unless this is a dry run.
fn run_task_agent(cfg: &Config, label: &str, packet: &str, dry_run: bool) -> TaskAgentRun {
    let ctx = target_ctx(cfg, None, None);
    let catalog = bn_core::agents::discover(cfg, ctx.as_ref())
        .unwrap_or_else(|error| fail(&format!("bn task {label}: {error}")));
    let agent = catalog
        .require(label)
        .unwrap_or_else(|error| fail(&format!("bn task {label}: {error}")));
    let mut args = serde_json::Map::new();
    args.insert(
        "packet".into(),
        serde_json::Value::String(packet.to_string()),
    );
    let prompt = agent
        .render(Some(&args))
        .unwrap_or_else(|error| fail(&format!("bn task {label}: {error}")));
    if dry_run {
        return TaskAgentRun {
            prompt,
            launched: false,
            output: String::new(),
        };
    }

    let harness = agent.harness.as_deref().unwrap_or("copilot");
    let Some(cmd) =
        crate::cmd::staff::staff_dispatch_command(harness, &prompt, agent.model.as_deref(), None)
    else {
        fail(&format!(
            "bn task {label}: unknown harness '{harness}' (use copilot | claude)"
        ));
    };
    let (success, output, combined) = crate::cmd::staff::dispatch_capture(cmd, None);
    if !success {
        fail(&format!(
            "bn task {label}: harness failed: {}",
            bn_core::staff::failure_detail(&combined)
        ));
    }
    TaskAgentRun {
        prompt,
        launched: true,
        output,
    }
}

/// `bn task enhance <id>` — make one task executable.
pub(crate) fn emit_task_enhance(
    cfg: &Config,
    id: Option<&str>,
    steer: Option<&str>,
    due: bool,
    dry_run: bool,
    as_json: bool,
) {
    let path = task_path(cfg);
    let file = bn_core::todo::load(&path).unwrap_or_else(|e| fail(&format!("bn task: {e}")));
    let today = today();

    let task = match (id, due) {
        (Some(id), _) => file
            .tasks()
            .find(|t| t.id() == Some(id))
            .cloned()
            .unwrap_or_else(|| fail(&format!("bn task enhance: no task {id}"))),
        // `--due` picks the work the gate actually rejected, highest-ranked first — the queue the
        // idle loop drains. A `stuck` task is excluded: three agents already failed at it, so it
        // is waiting on a person, not on another attempt.
        (None, true) => {
            let open: Vec<&bn_core::todo::Task> = file
                .tasks()
                .filter(|t| t.is_open(&today))
                .filter(|t| !matches!(t.meta("smart"), Some("ok") | Some("stuck")))
                .collect();
            match bn_core::task_agents::rank(&open, &today).first() {
                Some(t) => (*t).clone(),
                None => {
                    println!("Nothing needs improving — every open task has a verdict.");
                    return;
                }
            }
        }
        (None, false) => fail("bn task enhance: pass a task id, or --due for the next one"),
    };

    let task_id = task.id().unwrap_or("?").to_string();
    // Re-judge now so the packet names the *current* gaps rather than a stale stamp.
    let known = bn_core::issues::known_repos(cfg).into_iter().collect();
    let doc = bn_core::task_doc::read(cfg, &task);
    let verdict = bn_core::smart::check(&task, doc.as_ref(), &known, &today);
    if verdict.ok {
        println!("{task_id} already passes the SMART gate — nothing to improve.");
        return;
    }

    let captured = bn_core::task_agents::CapturedContext::read(cfg, &task);
    let packet = bn_core::task_agents::improve_packet(&task, Some(&verdict), steer, &captured);
    if !dry_run && !as_json {
        println!("Improving {task_id} — fails on: {}", verdict.codes().join(", "));
    }
    let run = run_task_agent(cfg, "task-enhance", &packet, dry_run);
    if as_json {
        let data = serde_json::json!({
            "id": task_id,
            "gaps": verdict.codes(),
            "prompt": run.prompt,
            "launched": run.launched,
            "output": run.output,
        });
        println!("{}", Envelope::success("task_enhance", data).to_json());
        return;
    }
    if dry_run {
        println!("{}", run.prompt);
    } else if !run.output.trim().is_empty() {
        println!("{}", run.output.trim());
    }
}

/// `bn task triage` — decide what is doable today.
pub(crate) fn emit_task_triage(cfg: &Config, day: Option<&str>, dry_run: bool, as_json: bool) {
    if !dry_run && !triage_allowed_here(cfg) {
        fail(&format!(
            "bn task triage: this machine is not the staff home ({}); triage rewrites the shared store, so it runs in one place",
            cfg.staff_home.as_deref().unwrap_or("?")
        ));
    }
    let day = day.map(str::to_string).unwrap_or_else(today);
    if bn_core::util::epoch_day(&day).is_none() {
        fail("bn task triage: --day must be YYYY-MM-DD");
    }

    let path = task_path(cfg);
    let file = bn_core::todo::load(&path).unwrap_or_else(|e| fail(&format!("bn task: {e}")));
    let open: Vec<&bn_core::todo::Task> = file.tasks().filter(|t| t.is_open(&day)).collect();
    if open.is_empty() {
        println!("No open tasks to triage — `bn task pull` to refill.");
        return;
    }
    let ranked = bn_core::task_agents::rank(&open, &day);
    let packet = bn_core::task_agents::triage_packet(&ranked, &day, None);
    let shortlist: Vec<serde_json::Value> = ranked
        .iter()
        .take(bn_core::task_agents::TRIAGE_WINDOW)
        .map(|t| {
            serde_json::json!({
                "id": t.id(),
                "summary": t.summary(),
                "score": bn_core::task_agents::pre_score(t, &day),
            })
        })
        .collect();

    if !as_json && !dry_run {
        println!("Ranked shortlist for {day} ({} open):", open.len());
        for t in ranked.iter().take(bn_core::task_agents::TRIAGE_WINDOW) {
            println!(
                "  {:>5}  {}  {}",
                bn_core::task_agents::pre_score(t, &day),
                t.id().unwrap_or("?"),
                t.summary()
            );
        }
    }
    let run = run_task_agent(cfg, "task-triage", &packet, dry_run);
    if as_json {
        let data = serde_json::json!({
            "day": day,
            "shortlist": shortlist,
            "prompt": run.prompt,
            "launched": run.launched,
            "output": run.output,
        });
        println!("{}", Envelope::success("task_triage", data).to_json());
    } else if dry_run {
        println!("{}", run.prompt);
    } else if !run.output.trim().is_empty() {
        println!("{}", run.output.trim());
    }
}

/// The task id governing `subject` in `repo`, minting one if nothing covers it yet (#1001).
///
/// Every enqueue must name a task — that invariant is what turns "work only ever starts from a
/// task" from a convention into something a test can assert. Rather than making callers refuse,
/// this mints the missing record so the ergonomic path stays one command: `bn orchestrate add
/// "..."` still works, it just leaves a durable trail behind it.
///
/// Idempotent on `key:enqueue:<hash of subject>`, so re-queuing the same subject reuses its task
/// instead of minting a second one.
pub(crate) fn task_for_enqueue(cfg: &Config, repo: &str, subject: &str, src: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    subject.trim().hash(&mut h);
    repo.hash(&mut h);
    let key = format!("enqueue:{:016x}", h.finish());

    let path = task_path(cfg);
    let _lock = bn_core::todo::lock(&path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(mut file) = bn_core::todo::load(&path) else {
        return String::new();
    };
    if let Some(existing) = file
        .tasks()
        .find(|t| t.meta("key") == Some(key.as_str()))
        .and_then(|t| t.id())
    {
        return existing.to_string();
    }

    let today = today();
    let id = file.next_id();
    // The subject may be free text from anywhere, so defang it before it becomes a description —
    // the same rule #1003 applies to issue titles.
    let text = bn_core::todo::neutralize_meta(subject);
    let mut task = bn_core::todo::Task::new_todo(&id, &text, None, "", src, &today);
    task.set_meta_raw("repo", repo);
    task.set_meta_raw("key", &key);
    file.push_task(task);
    if bn_core::todo::save(&path, &file).is_err() {
        return String::new();
    }
    id
}
