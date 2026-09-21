//! Native cron-job implementations. The name → fn table lives in [`crate::cmd::job_registry`].
use crate::*;
use std::cell::RefCell;

thread_local! {
    static NATIVE_JOB_OUTPUT: RefCell<String> = const { RefCell::new(String::new()) };
}

pub(crate) fn reset_native_job_output() {
    NATIVE_JOB_OUTPUT.with(|output| output.borrow_mut().clear());
}

pub(crate) fn take_native_job_output() -> String {
    NATIVE_JOB_OUTPUT.with(|output| std::mem::take(&mut *output.borrow_mut()))
}

pub(crate) fn native_job_line(line: &str, error: bool) {
    if error {
        eprintln!("{line}");
    } else {
        println!("{line}");
    }
    NATIVE_JOB_OUTPUT.with(|output| {
        let mut output = output.borrow_mut();
        output.push_str(line);
        output.push('\n');
    });
}

/// Run one cadence tick: every job in `children`, in order, through the normal `bn run` path.
///
/// These clusters used to be one systemd timer (and one `bn` process) per job, all firing on the
/// same schedule. Folding them behind a single timer costs no observability: each child still goes
/// through [`run_native_job`]/`run_job`, so it records its **own** `cron_run` event — `bn cron` and
/// `bn doctor` show the same per-job rows and per-job failure counts as before. It also preserves
/// each child's host gate, since `run_job` applies `job_allowed_here` per child.
///
/// A failing child never blocks its siblings, and the tick itself returns 0 even when one fails:
/// the child already recorded its own failure, so failing the parent too would double-report the
/// same problem and mask which job actually broke.
fn run_tick(cfg: &Config, ctx: Option<&Resolved>, tick: &str, children: &[&str]) -> i32 {
    let mut failed: Vec<&str> = Vec::new();
    for job in children {
        if crate::cmd::cron::run_job(cfg, ctx, job) != 0 {
            failed.push(job);
        }
    }
    if failed.is_empty() {
        native_job_line(&format!("{tick}: {} job(s) ok", children.len()), false);
    } else {
        native_job_line(
            &format!(
                "{tick}: {}/{} ok — failed: {}",
                children.len() - failed.len(),
                children.len(),
                failed.join(", ")
            ),
            true,
        );
    }
    0
}

/// `tick-5m` — the five-minute housekeeping cluster (see [`run_tick`]).
pub(crate) fn job_tick_5m(cfg: &Config, ctx: Option<&Resolved>) -> i32 {
    run_tick(cfg, ctx, "tick-5m", TICK_5M)
}

/// `tick-10m` — the ten-minute cluster (see [`run_tick`]).
pub(crate) fn job_tick_10m(cfg: &Config, ctx: Option<&Resolved>) -> i32 {
    run_tick(cfg, ctx, "tick-10m", TICK_10M)
}

/// `tick-15m` — the fifteen-minute cluster (see [`run_tick`]).
pub(crate) fn job_tick_15m(cfg: &Config, ctx: Option<&Resolved>) -> i32 {
    run_tick(cfg, ctx, "tick-15m", TICK_15M)
}

/// What the committer should do for one notes repo, given its git state. Pure so it's testable
/// without a real remote: `dirty` = uncommitted/untracked changes present, `only_state_churn` =
/// every dirty path is machine bookkeeping ([`is_state_churn_path`]), `has_upstream` = the branch
/// tracks a remote, `ahead` = local commits not yet pushed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NotesAction {
    /// Nothing uncommitted and nothing unpushed — no commit, no network.
    Skip,
    /// Dirty, but every changed path is machine state churn and there is nothing unpushed. No
    /// commit, no network: the churn rides along with the next real change instead.
    SkipStateChurn,
    /// Commit the dirty tree, but there's no upstream to push to (commit-only).
    CommitOnly,
    /// Commit if there's anything worth committing, then rebase-on-remote and push (also runs when
    /// only unpushed commits remain from a prior run that committed but couldn't push).
    CommitAndPush,
}

#[derive(Debug, PartialEq, Eq)]
enum RemoteRefState {
    Exists,
    Missing,
    QueryFailed,
}

/// Read tracking configuration without resolving the remote-tracking ref. `@{u}` stops resolving
/// after `fetch --prune` removes a deleted remote branch, but these two settings remain and are
/// exactly what lets commit-notes recreate that branch.
fn configured_upstream(
    git: &impl Fn(&std::path::Path, &[&str]) -> (bool, String),
    root: &std::path::Path,
    branch: &str,
) -> Option<(String, String)> {
    let remote_key = format!("branch.{branch}.remote");
    let merge_key = format!("branch.{branch}.merge");
    let (remote_ok, remote) = git(root, &["config", "--get", &remote_key]);
    let (merge_ok, merge_ref) = git(root, &["config", "--get", &merge_key]);
    (remote_ok && merge_ok && !remote.is_empty() && !merge_ref.is_empty())
        .then_some((remote, merge_ref))
}

/// Query the remote ref without `--exit-code`: a successful empty result proves absence, while a
/// non-zero exit remains distinguishable as an auth/network/invalid-remote failure.
fn remote_ref_state(root: &std::path::Path, remote: &str, merge_ref: &str) -> RemoteRefState {
    match crate::git::git_capture(
        &["ls-remote", "--heads", remote, merge_ref],
        Some(root),
        true,
    ) {
        Some(output) if output.is_empty() => RemoteRefState::Missing,
        Some(_) => RemoteRefState::Exists,
        None => RemoteRefState::QueryFailed,
    }
}


/// Whether a dirty tree holds anything a human would want to see in the notes history.
///
/// `only_state_churn` says every changed path is machine bookkeeping ([`is_state_churn_path`]):
/// the `state/running/<job>` marker that *this very run* rewrote, a `catchup-<date>` stamp,
/// `attention.json`, `doctor.json`. Committing that alone produces pure noise — the real example
/// is `1d5fc7b` in the notes repo, a whole commit whose entire content is one `+1` line in
/// `state/running/commit-notes`.
///
/// This is *piggyback*, not exclusion. The churn files are never held back from staging: the
/// moment any real change exists they are committed along with it by the usual `git add -A`,
/// because other hosts read them and permanently withholding them would be a sync regression. All
/// this decides is whether churn *by itself* justifies writing a commit. It doesn't.
pub(crate) fn notes_worth_committing(dirty: bool, only_state_churn: bool) -> bool {
    dirty && !only_state_churn
}

pub(crate) fn notes_action(
    dirty: bool,
    only_state_churn: bool,
    has_upstream: bool,
    ahead: u64,
) -> NotesAction {
    // `ahead > 0` still earns the network trip even on a churn-only day: those are real commits a
    // previous run made but couldn't push, and stranding them locally is the failure mode this job
    // exists to prevent.
    if !notes_worth_committing(dirty, only_state_churn) && ahead == 0 {
        return if dirty {
            NotesAction::SkipStateChurn
        } else {
            NotesAction::Skip
        };
    }
    if has_upstream {
        NotesAction::CommitAndPush
    } else {
        NotesAction::CommitOnly
    }
}

/// The `state/` subtree segment every machine's bookkeeping lives under, as porcelain prints it
/// (`<machine>/state/…`, always `/`-separated regardless of platform).
const STATE_SEGMENT: &str = "/state/";

/// State paths whose *prefix* marks them as churn: the `running/<job>` markers rewritten by every
/// cron run (including this one), and the `catchup-<date>` stamps the scheduler drops.
const CHURN_STATE_PREFIXES: &[&str] = &["running/", "catchup-"];

/// State files rewritten on a schedule whose content is a regenerated snapshot, not a decision.
const CHURN_STATE_FILES: &[&str] = &["attention.json", "doctor.json"];

/// Whether `path` (repo-relative, `/`-separated) is machine state churn — bookkeeping that changes
/// on its own cadence and is never, on its own, a reason to write a commit. See
/// [`notes_worth_committing`] for why this classifies rather than excludes.
pub(crate) fn is_state_churn_path(path: &str) -> bool {
    let Some(idx) = path.find(STATE_SEGMENT) else {
        return false;
    };
    let under_state = &path[idx + STATE_SEGMENT.len()..];
    CHURN_STATE_PREFIXES
        .iter()
        .any(|p| under_state.starts_with(p))
        || CHURN_STATE_FILES.contains(&under_state)
}

/// The only frontmatter lists whose entries do **not** count as content.
///
/// Every other populated list makes the note real — a note's `todos`/`blockers`/`decisions`/
/// `research`/`collab`/`ask`/`done_when`/`tags`/`collaborators`, a daily's `goals`/`learnings`/
/// `reflections`, the follow-up queue's `scheduled`, and anything a later field adds. Stating the
/// exceptions rather than the members is deliberate: a new note field then arrives protected, and
/// the failure mode of forgetting to update this list is a scaffold that survives, not writing
/// that gets deleted.
///
/// These two are the exceptions because `Frontmatter::record_session` stamps them merely because a
/// harness touched the note — exactly the "I opened a worktree" provenance that produced the
/// scaffold in the first place, so they say nothing about whether anyone wrote anything.
const PROVENANCE_LIST_FIELDS: &[&str] = &["harnesses", "models"];

/// Frontmatter scalars that are `Option`s skipped while unset, so their mere presence means
/// something real was recorded against the branch (a PR, a work item, a pipeline stage, a note
/// type). Treating those as content costs us nothing and stops the sweep from throwing away the
/// one line that tied a branch to its PR.
const CONTENT_SCALAR_FIELDS: &[&str] = &["pr", "work_item", "stage", "type"];

/// CommonMark allows a block-level construct up to three leading spaces; the fourth makes it an
/// indented code block, which is content.
const MAX_BLOCK_INDENT: usize = 3;

/// Whether `text` — a markdown note's full contents — carries nothing a human wrote.
///
/// The rule, stated once: **anything that only contains headings is empty.** Merely entering a
/// worktree makes bn scaffold `<machine>/<repo>/<branch>/note.md`, and that scaffold is a
/// frontmatter block of empty lists followed by `# repo / branch`, `## Goal`, `## Progress` and
/// nothing else. Committing those is how one auto-checkpoint (`62d2f16` in the notes repo) landed
/// a batch of note files at +23 lines apiece that said nothing at all. Daily files fail the same
/// way: a `# Daily — <date>` header with no goals under it.
///
/// A note is content-empty when BOTH hold:
///
/// 1. every frontmatter list field is empty or absent ([`frontmatter_lists_are_empty`]) — one real
///    todo makes the note real even when the body is nothing but headings, which is the ordinary
///    shape of an actively-worked branch note; and
/// 2. every body line outside the frontmatter is blank, an ATX heading, or a horizontal rule
///    ([`body_is_only_structure`]). One line of prose, one list item, one table row, one line of
///    code, and the note is real.
///
/// Pure — `&str` in, `bool` out, no filesystem — so the rule can be read and argued with in one
/// place, and the caller alone decides what to do about the answer.
pub(crate) fn markdown_is_content_empty(text: &str) -> bool {
    let Some((front, body)) = split_leading_frontmatter(text) else {
        // An opened-but-never-closed fence is malformed. "I couldn't parse it" must never read as
        // "it's empty" — that's the one way this predicate could destroy something real.
        return false;
    };
    frontmatter_lists_are_empty(front) && body_is_only_structure(body)
}

/// Split a leading `---` … `---` YAML block off `text` into `(frontmatter, body)`. Text with no
/// fence at all yields an empty frontmatter and the whole text as body. Returns `None` only for a
/// fence that opens and never closes, which callers must treat as "unknown", not "empty".
///
/// A local, forgiving splitter rather than a YAML parse: this predicate has to give an answer for a
/// half-written note too, and it only ever needs to see line shapes.
fn split_leading_frontmatter(text: &str) -> Option<(&str, &str)> {
    let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return Some(("", text));
    };
    let mut consumed = 0usize;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            return Some((&rest[..consumed], &rest[consumed + line.len()..]));
        }
        consumed += line.len();
    }
    None
}

/// Whether every content-bearing list in `front` is empty or absent, and no
/// [`CONTENT_SCALAR_FIELDS`] scalar is set. Both YAML sequence spellings the notes store emits are
/// handled: the flow form the scaffold writes (`todos: []`) and the block form a populated note
/// writes (`todos:` followed by `- id: t1` rows). [`PROVENANCE_LIST_FIELDS`] are the exemptions.
fn frontmatter_lists_are_empty(front: &str) -> bool {
    // Set while we're directly under a content-bearing key that has nothing on its own line, so
    // the `- ` rows that follow — if it turns out to be a block sequence — attribute to it. Any
    // other top-level key ends that scope.
    let mut under_content_list = false;
    for line in front.lines() {
        if under_content_list && line.trim_start().starts_with('-') {
            return false; // a real item
        }
        let Some((key, value)) = top_level_field(line) else {
            continue; // an item's indented sub-key, a comment, a blank line
        };
        under_content_list = false;
        if PROVENANCE_LIST_FIELDS.contains(&key) {
            continue; // auto-stamped; its rows are not content
        }
        if CONTENT_SCALAR_FIELDS.contains(&key) && !value.is_empty() {
            return false;
        }
        if value.is_empty() {
            under_content_list = true; // a block sequence's rows, if any, are the next lines
        } else if value.starts_with('[') && !is_empty_flow_sequence(value) {
            return false; // `tags: [a, b]`
        }
    }
    true
}

/// A top-level `key: value` line's key and trimmed value, or `None` when the line isn't one (it is
/// indented, blank, a comment, or a sequence row). Indentation is the whole test for nesting here:
/// the notes store only ever writes top-level keys flush left.
fn top_level_field(line: &str) -> Option<(&str, &str)> {
    if line.starts_with([' ', '\t']) || line.trim_start().starts_with('#') {
        return None;
    }
    let (key, value) = line.split_once(':')?;
    if key.is_empty() || key.contains(char::is_whitespace) {
        return None;
    }
    Some((key, value.trim()))
}

/// Whether a YAML flow sequence is empty — `[]`, or `[ ]` with only whitespace inside.
fn is_empty_flow_sequence(value: &str) -> bool {
    value
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .is_some_and(|inner| inner.trim().is_empty())
}

/// Whether every line of `body` is pure structure: blank, an ATX heading, or a horizontal rule.
/// This is the "anything that only contains headings is empty" half of
/// [`markdown_is_content_empty`].
fn body_is_only_structure(body: &str) -> bool {
    body.lines().all(is_structural_line)
}

/// Whether one body line carries no content: blank, an ATX heading (`#`…`######`), or a thematic
/// break (`---`, `***`, `___`).
fn is_structural_line(line: &str) -> bool {
    let indent = line.len() - line.trim_start().len();
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }
    // Past three spaces of indent it is an indented code block, i.e. content, whatever it looks
    // like.
    indent <= MAX_BLOCK_INDENT && (is_atx_heading(trimmed) || is_thematic_break(trimmed))
}

/// Whether `trimmed` is an ATX heading: one to six `#`, then a space or end of line.
fn is_atx_heading(trimmed: &str) -> bool {
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hashes)
        && trimmed[hashes..]
            .chars()
            .next()
            .is_none_or(|c| c == ' ' || c == '\t')
}

/// Whether `trimmed` is a thematic break: three or more of `-`, `*`, or `_`, spaces allowed
/// between them and nothing else on the line.
fn is_thematic_break(trimmed: &str) -> bool {
    let Some(marker) = trimmed.chars().next().filter(|c| "-*_".contains(*c)) else {
        return false;
    };
    trimmed.chars().filter(|c| *c == marker).count() >= 3
        && trimmed
            .chars()
            .all(|c| c == marker || c == ' ' || c == '\t')
}

/// Filenames that live in a notes repo but are never per-branch notes, so they must survive the
/// sweep even while genuinely content-empty.
const NON_NOTE_FILES: &[&str] = &["README.md", "CLAUDE.md"];

/// Path segments whose entire subtree is scaffolding *for* notes rather than notes: templates are
/// supposed to be headings and nothing else, references and workflow docs are curated by hand.
const NON_NOTE_DIRS: &[&str] = &["templates", "references", ".github"];

/// Whether `path` (repo-relative and `/`-separated, as `git status --porcelain` prints it) is a
/// markdown note the committer is allowed to delete once it proves content-empty.
///
/// Deliberately narrow — the cost of a wrong `false` is one uselessly-committed scaffold, the cost
/// of a wrong `true` is someone's writing. So: markdown only, never a repo-root file (notes live
/// under a `<machine>/…` subtree; the root holds the README and other hand-written docs), and never
/// inside a template/reference/workflow subtree.
///
/// Deleting a genuinely empty note is safe because it isn't a loss: bn recreates the scaffold on
/// demand, so the next command run in that worktree writes the same lines straight back.
pub(crate) fn is_deletable_note_path(path: &str) -> bool {
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() < 2 {
        return false; // a repo-root file, not a note
    }
    let file = segments[segments.len() - 1];
    if !file.ends_with(".md") || NON_NOTE_FILES.contains(&file) {
        return false;
    }
    !segments.iter().any(|s| NON_NOTE_DIRS.contains(s))
}

/// A timestamped, host-tagged conventional message for an auto-checkpoint commit. Kept simple and
/// deterministic (no Claude round-trip) so the committer never hangs on an unattended run.
pub(crate) fn notes_commit_message(host: &str, stamp: &str) -> String {
    format!("notes: auto-checkpoint {stamp} ({host})")
}

/// `commit-notes` — auto-commit (and push, where an upstream exists) the branch-notes repos. This
/// is the cross-platform, bn-native committer that replaced the Linux-only systemd unit (#471): as
/// a native job it runs from `bn cron register`'s launchd agents on macOS and Windows tasks too,
/// so notes can't silently stop syncing on a platform without systemd.
///
/// Only attempts work when there's something to do (a clean tree with nothing unpushed is skipped —
/// no commit, no network). Before staging it prunes the content-empty scaffolds `git add -A` would
/// otherwise immortalise ([`prune_empty_notes`]), then decides whether what's left is worth a
/// commit at all — a tree dirtied only by machine state churn isn't ([`notes_worth_committing`]).
/// When it is, it stages everything (`git add -A`, so untracked dispatch note dirs land too) and
/// commits with a timestamped message. When the branch has an upstream it rebases on the remote
/// first (multi-host: several machines push here) so a push from another machine doesn't reject us,
/// then pushes. Never blocks on credentials. Returns non-zero if any repo's commit, fetch or
/// rebase failed; a push miss after a successful local commit is logged but left non-fatal so the
/// durable commit still counts as a green cron run.
///
/// Every line it prints goes through [`native_job_line`], so `bn cron show commit-notes` reports
/// what was pruned and why a repo was skipped rather than leaving it to be inferred from a diff.
pub(crate) fn job_commit_notes(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    use std::path::PathBuf;

    let host = cfg.machine();
    let stamp = bn_core::util::now_stamp();

    // Person-scoped stores that predate the work-root layout may still be sitting under
    // `personal_root`. Resolve them once up front so the first sync after upgrade moves them into
    // the git-backed notes root before we inspect what needs committing.
    let _ = cfg.tasks_file();
    let _ = cfg.tasks_dir();
    let _ = cfg.references_dir();
    let _ = cfg.links_file();

    // `git -C <dir> <args...>` → (success, trimmed stdout), routed through the shared git seam
    // (`crate::git`) so it stays testable and the raw-subprocess guard is satisfied. A commit/push
    // that succeeds yields `(true, "")` (empty stdout); a failure yields `(false, "")`.
    let git = |dir: &std::path::Path, args: &[&str]| -> (bool, String) {
        match crate::git::git_capture(args, Some(dir), true) {
            Some(out) => (true, out),
            None => (false, String::new()),
        }
    };

    // Same seam, but keeping stderr, for the paths whose failure is reported to a human. Without
    // it a failed commit can only name the repo, never the reason.
    let git_full = |dir: &std::path::Path, args: &[&str]| -> (bool, String, String) {
        crate::git::git_result(args, Some(dir))
    };

    let mut rc = 0;
    let mut seen: Vec<PathBuf> = Vec::new();
    for dir in [cfg.personal_root.clone(), cfg.work_root.clone()] {
        if dir.as_os_str().is_empty() || !dir.exists() {
            continue;
        }
        let (ok, top) = git(&dir, &["rev-parse", "--show-toplevel"]);
        if !ok || top.is_empty() {
            continue; // not a git repo
        }
        let root = PathBuf::from(top);
        if seen.contains(&root) {
            continue; // dedupe: personal + work may share a root
        }
        seen.push(root.clone());

        // Self-heal any stale lock / interrupted rebase before touching the repo, so one crashed
        // run can't wedge notes-sync indefinitely (the chronic commit-notes failure trap).
        heal_stale_git_state(&root);

        // Prune before reading the dirty state, so the decision below sees the tree as it will be
        // committed: `git add -A` cannot tell a note someone wrote from one that was scaffolded by
        // merely entering a worktree, so the emptiness rule has to run first.
        prune_empty_notes(&git, &root);

        let changed = dirty_paths(&git, &root);
        let dirty = !changed.is_empty();
        // Deleting an *untracked* scaffold leaves nothing behind; deleting a *tracked* one leaves a
        // staged deletion, which is a real change and is committed like any other.
        let only_state_churn = dirty && changed.iter().all(|e| is_state_churn_path(&e.path));
        let branch = git(&root, &["rev-parse", "--abbrev-ref", "HEAD"]).1;
        let tracking_config = configured_upstream(&git, &root, &branch);
        let (resolved_upstream, upstream) =
            git(&root, &["rev-parse", "--abbrev-ref", "@{u}"]);
        let has_upstream = tracking_config.is_some() || resolved_upstream;
        let ahead: u64 = if resolved_upstream {
            git(&root, &["rev-list", "--count", "@{u}..HEAD"])
                .1
                .parse()
                .unwrap_or(0)
        } else if has_upstream {
            // A configured-but-pruned upstream has no local comparison ref. Treat it as pending
            // sync so a clean tree with commits from an earlier failed run still recreates it.
            1
        } else {
            0
        };

        match notes_action(dirty, only_state_churn, has_upstream, ahead) {
            NotesAction::Skip => {
                native_job_line(&format!("clean: {}", root.display()), false);
                continue;
            }
            NotesAction::SkipStateChurn => {
                native_job_line(
                    &format!(
                        "state churn only ({} path(s)); nothing worth committing: {}",
                        changed.len(),
                        root.display()
                    ),
                    false,
                );
                continue;
            }
            NotesAction::CommitOnly => {
                if commit_all(&git_full, &root, host, &stamp) {
                    native_job_line(
                        &format!("no upstream; commit-only: {}", root.display()),
                        false,
                    );
                } else {
                    rc = 1;
                }
                continue;
            }
            NotesAction::CommitAndPush => {
                let committed_this_run = notes_worth_committing(dirty, only_state_churn);
                if committed_this_run && !commit_all(&git_full, &root, host, &stamp) {
                    rc = 1;
                    continue;
                }
                // Prefer the durable branch config because `@{u}` no longer resolves after prune.
                // The fallback only serves unusual repositories whose upstream resolves despite
                // missing branch config.
                let upstream_target = tracking_config.clone().or_else(|| {
                    upstream.split_once('/').map(|(remote, remote_branch)| {
                        (remote.to_string(), format!("refs/heads/{remote_branch}"))
                    })
                });
                let Some((remote, merge_ref)) = upstream_target else {
                    native_job_line(
                        &format!(
                            "upstream configuration unreadable; local commit kept, push skipped: {} ({branch})",
                            root.display()
                        ),
                        true,
                    );
                    rc = 1;
                    continue;
                };
                let ready_to_push = match remote_ref_state(&root, &remote, &merge_ref) {
                    RemoteRefState::Missing => true,
                    RemoteRefState::Exists => {
                        // Integrate only when the remote actually has commits this clone lacks.
                        // The notes repo is written continuously by other bn processes, so
                        // `pull --rebase --autostash` on an already-current branch is pure risk:
                        // the autostash can fail to reapply, which aborts the run and leaves the
                        // stash behind (five had accumulated on a real host) while achieving
                        // nothing, because there was never anything to rebase onto.
                        //
                        // Fetch into the tracking ref explicitly rather than reading `FETCH_HEAD`,
                        // which is shared state another git operation in the same repo may have
                        // written.
                        let remote_branch =
                            merge_ref.strip_prefix("refs/heads/").unwrap_or(&merge_ref);
                        let tracking = format!("refs/remotes/{remote}/{remote_branch}");
                        let refspec = format!("+{merge_ref}:{tracking}");
                        if !git(&root, &["fetch", "--quiet", &remote, &refspec]).0 {
                            native_job_line(
                                &format!(
                                    "fetch failed (auth/network?); local commit kept, push skipped: {} ({branch})",
                                    root.display()
                                ),
                                true,
                            );
                            rc = 1;
                            continue;
                        }
                        let behind: u64 = git(
                            &root,
                            &["rev-list", "--count", &format!("HEAD..{tracking}")],
                        )
                        .1
                        .parse()
                        .unwrap_or(0);
                        behind == 0 || git(&root, &["rebase", "--autostash", "-q", &tracking]).0
                    }
                    RemoteRefState::QueryFailed => {
                        native_job_line(
                            &format!(
                                "upstream query failed (auth/network?); local commit kept, push skipped: {} ({branch})",
                                root.display()
                            ),
                            true,
                        );
                        rc = 1;
                        continue;
                    }
                };
                if ready_to_push {
                    if git(&root, &["push", "-q"]).0 {
                        native_job_line(&format!("pushed: {} ({branch})", root.display()), false);
                    } else {
                        native_job_line(
                            &format!(
                                "push failed (auth/network?); local commit kept: {} ({branch})",
                                root.display()
                            ),
                            true,
                        );
                        if !committed_this_run {
                            rc = 1;
                        }
                    }
                } else {
                    let _ = git(&root, &["rebase", "--abort"]);
                    native_job_line(
                        &format!(
                            "rebase failed (conflict?); local commit kept, push skipped: {} ({branch})",
                            root.display()
                        ),
                        true,
                    );
                    rc = 1;
                }
            }
        }
    }
    rc
}

/// Stage everything (`git add -A`) and commit with a timestamped message. Returns false (and logs)
/// if the commit failed. Split out so both the commit-only and commit+push paths share it.
/// How old `.git/index.lock` must be before commit-notes treats it as abandoned. A notes
/// auto-committer never legitimately holds the index for minutes, so a lock older than this is a
/// crashed/killed git — clearing it is what stops a single dead run from silently wedging
/// notes-sync forever (the exact stall that ran commit-notes 4/5 failed on a real host).
pub(crate) const STALE_LOCK_AGE: std::time::Duration = std::time::Duration::from_secs(120);

/// True when `.git/index.lock`'s age means it is an abandoned lock we should clear: at or past
/// [`STALE_LOCK_AGE`], or of unknown age (no mtime → treat as stale rather than wedge forever).
pub(crate) fn index_lock_is_stale(age: Option<std::time::Duration>) -> bool {
    age.map(|a| a >= STALE_LOCK_AGE).unwrap_or(true)
}

/// Self-heal the stale git state that otherwise wedges *every* subsequent commit-notes run: an
/// abandoned `.git/index.lock` (older than [`STALE_LOCK_AGE`]) and a leftover interrupted rebase
/// (`rebase-merge`/`rebase-apply`, left behind when a prior run was killed mid-`pull --rebase`).
/// Both are self-heal but never silent — each action is printed. Returns the messages (for tests).
pub(crate) fn heal_stale_git_state(root: &std::path::Path) -> Vec<String> {
    let git_dir = root.join(".git");
    // A worktree's `.git` is a file, and a non-repo has none — nothing to heal in either case.
    if !git_dir.is_dir() {
        return Vec::new();
    }
    let mut healed = Vec::new();

    let lock = git_dir.join("index.lock");
    if let Ok(md) = std::fs::metadata(&lock) {
        let age = md.modified().ok().and_then(|m| m.elapsed().ok());
        if index_lock_is_stale(age) && std::fs::remove_file(&lock).is_ok() {
            let msg = format!("healed: cleared stale index.lock: {}", root.display());
            println!("{msg}");
            healed.push(msg);
        }
    }

    // An interrupted rebase leaves these dirs; the commit-notes rebase path aborts on its own
    // failures, so a leftover means a crash/kill. Abort to restore HEAD, then force-remove if the
    // abort itself couldn't (e.g. an untracked file blocked the checkout).
    for name in ["rebase-merge", "rebase-apply"] {
        let dir = git_dir.join(name);
        if dir.exists() {
            let _ = crate::git::git_capture(&["rebase", "--abort"], Some(root), true);
            let _ = std::fs::remove_dir_all(&dir);
            if !dir.exists() {
                let msg = format!(
                    "healed: cleared interrupted rebase ({name}): {}",
                    root.display()
                );
                println!("{msg}");
                healed.push(msg);
            }
        }
    }
    healed
}

fn commit_all(
    git: &impl Fn(&std::path::Path, &[&str]) -> (bool, String, String),
    root: &std::path::Path,
    host: &str,
    stamp: &str,
) -> bool {
    git(root, &["add", "-A"]);
    let msg = notes_commit_message(host, stamp);
    let (ok, out, err) = git(root, &["commit", "-q", "-m", &msg]);
    if ok {
        native_job_line(&format!("committed: {} — {msg}", root.display()), false);
        return true;
    }

    // `git commit` also exits non-zero when there is simply nothing staged. The notes repo is
    // written continuously by other bn processes, so a concurrent writer can commit the very
    // paths this run saw as dirty between the status snapshot and here. That is a benign no-op,
    // not a sync failure — deciding from the tree itself keeps this locale-independent rather
    // than matching git's "nothing to commit" wording.
    if git(root, &["status", "--porcelain"]).1.is_empty() {
        native_job_line(
            &format!(
                "nothing to commit (already captured by a concurrent run): {}",
                root.display()
            ),
            false,
        );
        return true;
    }

    let reason = [err.as_str(), out.as_str()]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or("no output from git");
    native_job_line(
        &format!("commit failed: {} — {reason}", root.display()),
        true,
    );
    false
}

/// One `git status --porcelain` row: its two-letter status and the repo-relative path.
struct DirtyPath {
    status: String,
    path: String,
}

/// The porcelain status letter for a path git already considers gone (staged or unstaged
/// deletion) — there is nothing left to read, let alone delete.
const PORCELAIN_DELETED: char = 'D';

/// The porcelain status for an untracked path. Git isn't holding it, so removing it is a plain
/// filesystem unlink rather than a `git rm`.
const PORCELAIN_UNTRACKED: &str = "??";

/// Every changed or untracked path in `root`, as `git status --porcelain` reports it.
fn dirty_paths(
    git: &impl Fn(&std::path::Path, &[&str]) -> (bool, String),
    root: &std::path::Path,
) -> Vec<DirtyPath> {
    git(root, &["status", "--porcelain"])
        .1
        .lines()
        .filter_map(parse_porcelain_line)
        .collect()
}

/// Parse one porcelain row (`XY <path>`). Renames and copies print `XY <old> -> <new>`; the
/// destination is the path that exists now, so that's the one we keep. Paths containing quotes or
/// control characters are printed C-quoted, and those are dropped rather than unescaped by hand —
/// the only cost of skipping one is a scaffold that lives another day.
fn parse_porcelain_line(line: &str) -> Option<DirtyPath> {
    if line.len() < 4 || !line.is_char_boundary(2) {
        return None;
    }
    let (status, rest) = line.split_at(2);
    let path = rest.trim_start();
    let path = path.rsplit(" -> ").next()?;
    if path.is_empty() || path.starts_with('"') {
        return None;
    }
    Some(DirtyPath {
        status: status.to_string(),
        path: path.to_string(),
    })
}

/// Delete the content-empty notes among `root`'s changed paths, before anything is staged.
///
/// This is the deletion half of the scaffold problem: `git add -A` will happily commit a note that
/// exists only because someone entered a worktree, and once committed it stays. Removing it is
/// preferred over merely leaving it unstaged — an unstaged scaffold just gets re-offered every
/// morning — and it costs nothing, because bn writes the same scaffold back on demand the next time
/// that branch is touched.
///
/// Scoped by [`is_deletable_note_path`] and gated on [`markdown_is_content_empty`], and never
/// silent: each removal (and each failed removal) prints a line, so `bn cron show commit-notes`
/// lists exactly what went. Returns how many were removed.
fn prune_empty_notes(
    git: &impl Fn(&std::path::Path, &[&str]) -> (bool, String),
    root: &std::path::Path,
) -> usize {
    let mut removed = 0;
    for entry in dirty_paths(git, root) {
        if !is_deletable_note_path(&entry.path) || entry.status.contains(PORCELAIN_DELETED) {
            continue;
        }
        let full = root.join(&entry.path);
        let Ok(text) = std::fs::read_to_string(&full) else {
            continue; // unreadable is not the same as empty
        };
        if !markdown_is_content_empty(&text) {
            continue;
        }
        let gone = if entry.status == PORCELAIN_UNTRACKED {
            std::fs::remove_file(&full).is_ok()
        } else {
            // `git rm` also stages the deletion, so it lands in this run's commit.
            git(root, &["rm", "-f", "--", &entry.path]).0
        };
        if gone {
            native_job_line(&format!("removed empty note: {}", entry.path), false);
            removed += 1;
        } else {
            native_job_line(&format!("could not remove empty note: {}", entry.path), true);
        }
    }
    removed
}

/// `watch` — one pass of the wake reactor (the cron form of `bn watch --once`). Toasts anything
/// newly needing you (agents waiting/blocked/stuck, agent completions, urgent attention) since the
/// last run; quiet when nothing changed. Native so it toasts identically on every platform.
pub(crate) fn job_watch(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    emit_watch(cfg, true, 30, bn_core::agent::DEFAULT_STUCK_MINS, false);
    0
}

/// `recall-index` — keep the machine-local semantic-recall index warm (#249): (re)embed only the
/// notes/investigation docs whose fingerprint changed since the last run, so it's cheap on a
/// schedule. The cron form of `bn recall --index`; fully offline and idempotent.
pub(crate) fn job_recall_index(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    emit_recall(cfg, "", None, true, 20, false);
    0
}

/// `managed-repos` — clone + refresh the repos listed in `[managed_repos]` under the bn-managed
/// root (`~/.bn/repos`), using bn's own bare+worktree layout so `wt`/`tat` can pick them up. This
/// is how dotfiles (and any always-checked-out repo) land on a new machine without a manual
/// `git clone`. Idempotent: clone-if-absent, else fetch + fast-forward each worktree. Best-effort —
/// a single repo failing never aborts the rest, so it always exits 0 (matching the shell job it
/// replaced). Native so the git fan-out runs identically on Linux/macOS/Windows.
pub(crate) fn job_managed_repos(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let root = resolver::managed_repos_dir();
    if root.as_os_str().is_empty() {
        eprintln!("managed-repos: empty managed root");
        return 1;
    }
    let bare_root = root.join("bare");

    for url in &cfg.managed_repos {
        if url.is_empty() {
            continue;
        }
        let name = url.rsplit('/').next().unwrap_or(url);
        let name = name.strip_suffix(".git").unwrap_or(name);
        let bare = bare_root.join(format!("{name}.git"));

        if bare.is_dir() {
            let bare_s = bare.to_string_lossy();
            if !crate::git::git_ok(&[
                "--git-dir",
                &bare_s,
                "fetch",
                "--quiet",
                "--prune",
                "origin",
            ]) {
                eprintln!("managed-repos: fetch failed for {name}");
            }
            fast_forward_worktrees(&bare_s);
            println!("managed-repos: refreshed {name}");
        } else {
            println!("managed-repos: cloning {name}…");
            // Pin the clone under the managed root (mirrors the shell job's `WT_ROOT=$root`), but
            // via an explicit-root arg rather than mutating process env.
            if wt::cmd_clone_root(url, &root) {
                println!("managed-repos: cloned {name} -> {}", bare.display());
            } else {
                eprintln!("managed-repos: clone failed for {name} ({url})");
            }
        }
    }
    0
}

/// Fast-forward every worktree of a managed bare repo to its `origin/<branch>` counterpart
/// (best-effort — dirty or diverged worktrees are skipped, and a failed merge is ignored). Mirrors
/// the shell job's per-worktree `git merge --ff-only origin/$branch` fan-out.
fn fast_forward_worktrees(bare_s: &str) {
    let listing = crate::git::git_capture(
        &["--git-dir", bare_s, "worktree", "list", "--porcelain"],
        None,
        true,
    )
    .unwrap_or_default();

    // Parse `worktree <path>` / `branch refs/heads/<name>` pairs; detached/bare entries (no branch
    // line) are skipped.
    let mut path: Option<String> = None;
    for line in listing.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(p.trim().to_string());
        } else if let Some(b) = line.strip_prefix("branch ") {
            let branch = b.trim().strip_prefix("refs/heads/").unwrap_or(b.trim());
            if let Some(p) = path.take() {
                if std::path::Path::new(&p).is_dir() {
                    let _ = crate::git::git_ok(&[
                        "-C",
                        &p,
                        "merge",
                        "--ff-only",
                        &format!("origin/{branch}"),
                    ]);
                }
            }
        } else if line.trim().is_empty() {
            path = None;
        }
    }
}

/// `refresh-all` — the daily keep-current sweep. Clones/fast-forwards the bn-managed repos
/// (`[managed_repos]`, so dotfiles & co. stay current) and reconciles the cron timers with the
/// manifest (so a `crons.toml` change pulled into the repo takes effect without a manual
/// `bn cron register`). Both steps are idempotent and best-effort; always exits 0.
///
/// Note: the shell job it replaced also ran `bn refresh-all` up front, but no such subcommand ever
/// existed — that line errored to a no-op — so the native handler drops it and does the two real
/// steps only (#485).
pub(crate) fn job_refresh_all(cfg: &Config, ctx: Option<&Resolved>) -> i32 {
    let _ = job_managed_repos(cfg, ctx);
    emit_cron_register(cfg, ctx, false, false);
    0
}

/// `tools-reconcile` cron (#653) — the keep-alive loop for the managed "tools" subsystem: for
/// every declared docker-backed tool (opted-in built-ins + `<config_dir>/tools.toml`), re-assert
/// the "up" desired state (create if absent, start if stopped, no-op if running). Best-effort per
/// tool — one failing tool doesn't stop the others; the exit code is non-zero only if at least one
/// tool couldn't be brought up, so the scheduler / `bn cron` surfaces the stall.
pub(crate) fn job_tools_reconcile(_cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    use bn_core::runner::RealRunner;
    use bn_core::tools::{self, DeployOutcome};
    let specs = tools::all_specs();
    if specs.is_empty() {
        native_job_line("tools-reconcile: no tools declared", false);
        return 0;
    }
    // All managed tools are docker-backed. On a host with no docker on PATH (a builder without
    // Docker Desktop, a WSL box with the daemon down) every `docker inspect`/`run` fails, so the
    // job would return non-zero on EVERY tick forever — the chronic-failure trap (539/540 silent
    // fails). Skip cleanly instead: nothing to reconcile without a daemon isn't a failure.
    let path = std::env::var("PATH").unwrap_or_default();
    if !bn_core::mcp::command_on_path("docker", &path)
        && !bn_core::mcp::command_on_path("docker.exe", &path)
    {
        println!(
            "tools-reconcile: docker not on PATH — skipping ({} tool(s))",
            specs.len()
        );
        return 0;
    }
    let mut rc = 0;
    for spec in &specs {
        match tools::reconcile(&RealRunner, spec) {
            DeployOutcome::Created => {
                native_job_line(&format!("tools-reconcile: {} created", spec.name), false)
            }
            DeployOutcome::Started => {
                native_job_line(&format!("tools-reconcile: {} started", spec.name), false)
            }
            DeployOutcome::AlreadyRunning => native_job_line(
                &format!("tools-reconcile: {} already running", spec.name),
                false,
            ),
            DeployOutcome::Failed(e) => {
                native_job_line(
                    &format!("tools-reconcile: {} FAILED — {e}", spec.name),
                    true,
                );
                rc = 1;
            }
        }
    }
    rc
}

/// Run a native job with the same bookkeeping as the script path: the per-machine `only_on` gate,
/// the `bn cron running` marker, and a `cron_run` event with duration + exit code. Native jobs are
/// global scope (they live in the binary, not a note dir), so the event is tagged `global`. The
/// native jobs stream straight to stdout/stderr; jobs that opt into [`native_job_line`] also
/// retain the same text in the event tail for `bn cron show` / `bn cron failures`.
pub(crate) fn run_native_job(
    cfg: &Config,
    ctx: Option<&Resolved>,
    job: &str,
    f: impl FnOnce(&Config, Option<&Resolved>) -> i32,
) -> i32 {
    crate::cmd::cron::run_recorded_job(
        cfg,
        ctx,
        job,
        resolver::JobScope::Global,
        || {
            reset_native_job_output();
            let code = f(cfg, ctx);
            let tail = tail_lines(&take_native_job_output(), 15);
            (code, tail)
        },
    )
}

/// `staff-archive` cron — the #275 store-retention sweep: compact fully-elapsed quarters of feed
/// cards into a per-quarter headline index and roll up `staff_run` ledger rows older than a year
/// into monthly aggregates. Idempotent, so a weekly (or caught-up-after-a-miss) run is always safe;
/// chronicles and retro transcripts are exempt. Always exits 0 — retention is best-effort
/// housekeeping, never a page.
/// `jobs/wt-cleanup.sh` — reclaim finished worktrees: merged-PR ships (cleanup), then done+clean
/// scouts (reap). Both sweep all repos; the reap keeps the note (findings) while removing the
/// disposable scout worktree, so investigation worktrees don't pile up.
pub(crate) fn job_wt_cleanup(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    // #534: clear orphaned merged-but-dirty notices whose worktrees were reaped out of band before
    // reclaiming the current crop — decoupled from the single `cleanup_one` removal path.
    wt::reconcile_stuck(cfg);
    let cleanup = wt::run_code(
        cfg,
        wt::WtCmd::Cleanup {
            all: true,
            force: false,
            worktree: None,
        },
    );
    let reap = wt::run_code(
        cfg,
        wt::WtCmd::Reap {
            all: true,
            yes: true,
        },
    );
    cleanup.max(reap)
}

/// `fleet-reap` (#503) — the fleet garbage-collector: terminate orphaned/stale agent process trees
/// (window gone / worktree removed / PR merged/closed) and prune dead tmux windows. This is what
/// makes the reaper automatic on macOS, where the systemd `wt-cleanup` timer never fires (#471):
/// `bn cron register` materializes it as a launchd timer. Acts (kills), checks PR state (the cron
/// can afford the network), and honors every safety gate — never a working / WIP / orchestrator
/// agent.
pub(crate) fn job_fleet_reap(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    reap::run_reap(
        cfg,
        reap::ReapOpts {
            kill: true,
            check_pr: true,
            ..reap::ReapOpts::default()
        },
    )
}

/// `jobs/attention-sync.sh` — recompute + rewrite the attention cache (`bn next --refresh`).
pub(crate) fn job_attention_sync(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    // Retire items whose PR has merged or closed before refreshing, so a dead PR's "ping
    // reviewers" nag doesn't survive in the cache the panel reads (#966).
    let retired = bn_core::attention::retire_dead_pr_items(&bn_core::daily::dead_prs(cfg), &today());
    for item in &retired {
        println!("attention-sync: retired {} — {}", item.id, item.title);
    }
    // refresh=true, no_momentum=true: the cache write is always punch-free (momentum is a
    // read-time re-rank), so the sync job never bakes the current punch into the snapshot.
    emit_next(cfg, 5, false, false, true, false, true, false, false);
    println!("attention-sync: attention cache refreshed");
    0
}

/// `jobs/daily-rollover.sh` — roll today's daily goals forward (`bn daily rollover`).
pub(crate) fn job_daily_rollover(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    match bn_core::daily::rollover(cfg, &today()) {
        Ok(r) => {
            print!("{}", payload::rollover_text(&r));
            0
        }
        Err(e) => {
            eprintln!("daily-rollover: {e}");
            1
        }
    }
}

/// `jobs/metrics-snapshot.sh` — write today's scorecard snapshot (`bn metrics --snapshot`). Calls
/// the core writer directly (rather than `emit_metrics`) so a failure is recorded, not `exit`ed.
pub(crate) fn job_metrics_snapshot(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    use bn_core::clock::Clock;
    let clock = bn_core::clock::RealClock;
    let now_today = clock.today();
    let s = bn_core::metrics::scorecard(
        cfg,
        &bn_core::runner::RealRunner,
        clock.now_epoch_secs(),
        30,
        &now_today,
    );
    match bn_core::metrics::write_snapshot(cfg, &s, &now_today) {
        Ok(p) => {
            println!("metrics-snapshot: snapshot written: {}", p.display());
            0
        }
        Err(e) => {
            eprintln!("metrics-snapshot: {e}");
            1
        }
    }
}

/// The `gather-findings` cron (#897) — snapshot the local morning-planning caches into the
/// findings store ahead of `today-agent`. Runs the same offline adapters as `bn findings gather`
/// (PR review queue, your open PRs, inbound feed, stale/blocked branches, night-shift reviewables,
/// failing crons, sentinel, quarter goals, dojo cards), recording one durable run per non-empty
/// source. Additive and read-only over the caches: no network, no re-fetch, and no
/// feed/attention/daily side effect. A gather error is a non-fatal exit 1.
pub(crate) fn job_gather_findings(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    match bn_core::findings_gather::run(cfg) {
        Ok(report) => {
            native_job_line(
                &format!(
                    "gather-findings: {} source(s) recorded, {} item(s)",
                    report.recorded(),
                    report.total_items()
                ),
                false,
            );
            for s in report.sources.iter().filter(|s| s.recorded) {
                native_job_line(
                    &format!("  [{}] {} — {} item(s)", s.tier, s.source, s.items),
                    false,
                );
            }
            0
        }
        Err(e) => {
            native_job_line(&format!("gather-findings: {e}"), true);
            1
        }
    }
}

/// The `weekly-insights` cron — snapshot the current anchored week's `bn insights` retro into
/// `<notes>/<machine>/insights/<YYYY-Www>.md` so history accumulates and syncs. Sibling of
/// `metrics-snapshot`; writes directly (rather than `emit_insights`) so a failure is recorded,
/// not `exit`ed. Read-only over telemetry — never fetches.
pub(crate) fn job_weekly_insights(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    use bn_core::clock::Clock;
    let today = bn_core::clock::RealClock.today();
    let ins = bn_core::insights::insights(cfg, &today, None);
    let Some(week) = bn_core::util::iso_week(&ins.end) else {
        eprintln!("weekly-insights: can't derive the ISO week for {today}");
        return 1;
    };
    let path = cfg.insights_file(&week);
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("weekly-insights: {e}");
            return 1;
        }
    }
    match std::fs::write(&path, bn_core::insights::render(&ins)) {
        Ok(()) => {
            println!("weekly-insights: snapshot written: {}", path.display());
            0
        }
        Err(e) => {
            eprintln!("weekly-insights: {e}");
            1
        }
    }
}

/// `jobs/agent-observe.sh` — ingest agent session state (`bn agent ingest`).
pub(crate) fn job_agent_observe(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    emit_agent_ingest(cfg, false, false);
    0
}

/// `jobs/auto-review-poll.sh` — auto-review lifecycle poll (`bn review-pr poll`).
pub(crate) fn job_auto_review_poll(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    if review_pr::run(cfg, review_pr::ReviewPrCmd::Poll, false) {
        0
    } else {
        1
    }
}

/// `jobs/notify.sh` — offer each urgent attention item to the notification gate, then mark it
/// notified.
///
/// **`bn watch` owns attention delivery** (the `*:0/2` cron: faster, and it batches a burst into
/// one interrupt instead of one toast per item). This job therefore no longer rides `tick-5m` —
/// running both on their own cadences, each with its own dedup ledger, is what toasted every
/// urgent item twice. It stays registered so `bn run notify` and its cron-health row keep working,
/// and it keys deliveries `attn:<id>` exactly as watch does, so the shared ledger makes a
/// duplicate impossible even if the two ever run together again.
pub(crate) fn job_notify(_cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let items = bn_core::attention::to_notify(&today());
    if items.is_empty() {
        println!("notify: nothing new to push");
        return 0;
    }
    for it in &items {
        let body = match it.link.as_deref() {
            Some(l) if !l.is_empty() => l,
            _ => "urgent — needs attention",
        };
        // Urgent attention is blocking by definition: it is open, unacknowledged, and yours.
        let sent = crate::toast::toast(
            crate::toast::Class::Blocking,
            &format!("attn:{}", it.id),
            &it.source,
            &format!("bn: {}", it.title),
            body,
        );
        let _ = bn_core::attention::mark_notified(&it.id);
        println!(
            "notify: {} {} ({})",
            if sent { "toasted" } else { "held" },
            it.id,
            it.title
        );
    }
    0
}


/// `jobs/standup-push.sh` — copy today's standup line to the clipboard and toast it (autopilot).
pub(crate) fn job_standup_push(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let jc = resolver::JobConfig::load();
    if !jc.standup_enabled {
        println!("standup-push: standup autopilot disabled — nothing to push");
        return 0;
    }
    let s = bn_core::reports::standup(cfg, &today());
    let mut line = bn_core::reports::standup_line(&s);
    if jc.standup_fetch_delta {
        if let Some(d) = bn_core::reports::pr_delta(&bn_core::runner::RealRunner) {
            line.push_str(&format!(" · {d}"));
        }
    }
    if line.trim().is_empty() {
        println!("standup-push: nothing to push");
        return 0;
    }
    // The clipboard copy is the point of this job and never interrupts, so it runs regardless of
    // policy; the toast is a digest and stays off the toast channel under the default bar.
    bn_core::notify::clipboard(&line);
    crate::toast::toast(
        crate::toast::Class::Digest,
        &format!("standup:{}", today()),
        "standup",
        "bn standup",
        &line,
    );
    println!("{line}");
    0
}

/// `jobs/wrap-digest.sh` — end-of-day toast: the wrap digest plus a "still on you" attention tail.
pub(crate) fn job_wrap_digest(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    if !resolver::JobConfig::load().wrap_enabled {
        println!("wrap-digest: digest disabled — nothing to push");
        return 0;
    }
    let today = today();
    // Mirror emit_wrap: with no note in cwd, date un-noted commits at `today` so that section
    // stays empty rather than flagging every commit.
    let note_since = target_ctx(cfg, None, None)
        .and_then(|ctx| bn_core::store::read(&cfg.note_file(&ctx)).ok())
        .map(|note| bn_core::reports::note_last_activity(&note))
        .unwrap_or_else(|| today.clone());
    let w = bn_core::reports::wrap(cfg, &today, &note_since, &bn_core::runner::RealRunner);
    let mut digest = if w.is_empty() {
        String::new()
    } else {
        bn_core::reports::render_wrap(&w).trim_end().to_string()
    };

    if let Some(tail) = attn_source_tail(&bn_core::attention::open_items(&today)) {
        if digest.is_empty() {
            digest = tail;
        } else {
            digest.push('\n');
            digest.push_str(&tail);
        }
    }

    if digest.trim().is_empty() {
        println!("wrap-digest: clean day (or disabled) — nothing to push");
        return 0;
    }
    crate::toast::toast(
        crate::toast::Class::Digest,
        &format!("wrap:{today}"),
        "wrap",
        "bn wrap — end of day",
        &digest,
    );
    println!("{digest}");
    0
}

/// "Still on you: N emails, N Teams replies, N meetings" from open attention items, or `None` when
/// none of those sources are present. Replaces the jq pipeline in `wrap-digest.sh`.
pub(crate) fn attn_source_tail(items: &[bn_core::attention::AttnItem]) -> Option<String> {
    source_tail(items.iter().map(|i| i.source.as_str()))
}

/// The pure counting/pluralizing core of [`attn_source_tail`], over a stream of source names.
pub(crate) fn source_tail<'a>(sources: impl Iterator<Item = &'a str>) -> Option<String> {
    let (mut e, mut t, mut m) = (0usize, 0usize, 0usize);
    for s in sources {
        match s {
            "email" => e += 1,
            "teams" => t += 1,
            "meetings" => m += 1,
            _ => {}
        }
    }
    let mut parts = Vec::new();
    if e > 0 {
        parts.push(format!("{e} email{}", if e == 1 { "" } else { "s" }));
    }
    if t > 0 {
        parts.push(format!(
            "{t} Teams repl{}",
            if t == 1 { "y" } else { "ies" }
        ));
    }
    if m > 0 {
        parts.push(format!("{m} meeting{}", if m == 1 { "" } else { "s" }));
    }
    (!parts.is_empty()).then(|| format!("Still on you: {}", parts.join(", ")))
}

/// `jobs/auto-review-trigger.sh` — when idle, prep the next queued PR unattended.
pub(crate) fn job_auto_review_trigger(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    review_pr::auto_trigger(cfg)
}

/// The last `lines` lines of the active pane in the tmux window for `repo`/`branch`, matched by
/// the same canonical key the board join uses (glyph-stripped). `None` when the window is gone or
/// tmux is absent — the stuck-capture is best-effort.
pub(crate) fn pane_tail_for(repo: &str, branch: &str, lines: usize) -> Option<String> {
    let want = bn_core::attention::window_key_parts(repo, branch);
    let listing = crate::proc::tmux_raw(&[
        "list-windows",
        "-a",
        "-F",
        "#{window_id}\t#{session_name}\t#{window_name}",
    ])?;
    let wid = listing.lines().find_map(|l| {
        let c: Vec<&str> = l.split('\t').collect();
        let name = c
            .get(2)?
            .trim_start_matches(|ch: char| !ch.is_ascii_alphanumeric());
        (bn_core::attention::window_key_parts(c.get(1)?, name) == want)
            .then(|| c.first().map(|s| s.to_string()))?
    })?;
    // `-t <window_id>` captures the window's active pane.
    let text = crate::proc::tmux_raw(&["capture-pane", "-p", "-J", "-t", &wid])?;
    let all: Vec<&str> = text.lines().collect();
    let trimmed_len = all
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map(|p| p + 1)
        .unwrap_or(0);
    let start = trimmed_len.saturating_sub(lines);
    let tail = all[start..trimmed_len].join("\n");
    (!tail.trim().is_empty()).then_some(tail)
}

/// window). Delegates to [`emit_night_run`], which gates on the kill switch, `[night] enabled`, and
/// the window; its text output is captured in the run's event tail. Always exits 0 — a gate that
/// declines (kill switch, disabled, off-window) is a clean no-op, not a failure.
/// Fold each *stuck* in-flight item's pane tail into its `detail` — a wedged run explains itself
/// on the board instead of silently sitting there. Overwrites on each sweep (latest state wins);
/// best-effort throughout.
pub(crate) fn capture_stuck_details() {
    use bn_core::attention::WorkStatus;
    let (classified, _) = attn_lanes();
    for (s, st) in classified {
        if st != WorkStatus::Stuck {
            continue;
        }
        let (Some(repo), Some(branch)) = (s.item.repo.as_deref(), s.item.branch.as_deref()) else {
            continue;
        };
        if let Some(tail) = pane_tail_for(repo, branch, 15) {
            let detail = format!("stuck — pane tail:\n{tail}");
            let _ = bn_core::attention::update(
                &s.item.id,
                None,
                None,
                None,
                None,
                None,
                Some(&detail),
                None,
                None,
            );
            println!(
                "attention-auto-trigger: captured pane tail for stuck {}",
                s.item.id
            );
        }
    }
}

/// `attention-auto-trigger` — the autonomous pickup loop. When idle, fill every free concurrency
/// slot with the next eligible `auto` attention items (self-authored, prompted, with a repo/branch),
/// each launched with its agent. Concurrency is capped by `BN_ATTN_AUTO_MAX` (default 1 → one at a
/// time): `next_auto_batch` returns at most (cap − already-running) items, so the live fleet never
/// exceeds the cap; a *done* item awaiting review frees its slot, so the queue keeps draining across
/// sweeps. Also folds stuck runs' pane tails into their detail each sweep, so a wedged item explains
/// itself. The trust boundary (auto only for `source=manual`) lives in `next_auto_batch`, so no
/// untrusted input ever launches an unattended agent.
pub(crate) fn job_attention_auto_trigger(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let now = bn_core::util::now_epoch_secs();
    // Stuck-capture runs regardless of the idle gate — it's read-only on tmux and makes the board
    // truthful even while you're active.
    capture_stuck_details();
    let idle_mins = env_i64("BN_ATTN_AUTO_IDLE_MINS", 5);
    if !bn_core::idle::is_idle(cfg, now, idle_mins) {
        return 0; // active — don't interrupt
    }
    // Fill every free slot in one idle sweep, highest-priority first.
    let batch = bn_core::attention::next_auto_batch(cfg, &today(), attn_auto_max());
    if batch.is_empty() {
        return 0; // at the concurrency cap, or nothing eligible
    }
    let mut rc = 0;
    for item in &batch {
        println!("attention-auto-trigger: {} — {}", item.id, item.title);
        match crate::cmd::attn::run_attn_start_attributed(
            cfg,
            item,
            true,
            Some(bn_core::cost::WorkClass::Scheduled),
        ) {
            Some(o) if o.launched => {}
            Some(_) => {
                eprintln!(
                    "attention-auto-trigger: opened the window but the agent launch failed for {}",
                    item.id
                );
                rc = 1;
            }
            None => {
                eprintln!(
                    "attention-auto-trigger: could not open a worktree for {} ({}/{})",
                    item.id,
                    item.repo.as_deref().unwrap_or("?"),
                    item.branch.as_deref().unwrap_or("?")
                );
                rc = 1;
            }
        }
    }
    rc
}

/// `jobs/agent-supervise.sh` — report fleet health and, when `BN_SUPERVISE_NUDGE=1`, nudge stuck
/// agents via tmux send-keys. The jq selection of stuck agents becomes a Rust filter.
pub(crate) fn job_agent_supervise(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    let idle = env_i64("BN_SUPERVISE_IDLE_MINS", 5);
    let stuck_mins = env_i64("BN_SUPERVISE_STUCK_MINS", 15);
    if let Err(error) =
        agent_live::publish_actionable_wakes(cfg, idle, stuck_mins, bn_core::util::now_epoch_secs())
    {
        eprintln!("agent-supervise: wake queue: {error}");
        return 1;
    }
    let fleet = bn_core::agent::supervise(cfg, idle, stuck_mins);
    println!(
        "agent-supervise: {} running, {} stuck, {} ended (idle>{stuck_mins}m)",
        fleet.running_count(),
        fleet.stuck,
        fleet.ended
    );
    if fleet.stuck > 0 && std::env::var("BN_SUPERVISE_NUDGE").as_deref() == Ok("1") {
        let msg = std::env::var("BN_SUPERVISE_NUDGE_MSG").unwrap_or_else(|_| {
            "bn supervise: still there? continue with the current task, or commit + open a PR if it is done.".to_string()
        });
        for a in fleet
            .agents
            .iter()
            .filter(|a| a.health == bn_core::agent::AgentHealth::Stuck)
        {
            if tmux_send_keys(&a.target, &msg) {
                println!("agent-supervise: nudged {}", a.target);
            } else {
                eprintln!("agent-supervise: could not nudge {}", a.target);
            }
        }
    }
    0
}

/// Type `msg` then Enter into a tmux window (`bn agent send` mechanics) — true on delivery.
/// Exact-name target ([`tmux_exact_target`]) so a renumbered/prefix-matched window can't receive
/// another agent's nudge.
pub(crate) fn tmux_send_keys(target: &str, msg: &str) -> bool {
    let target = tmux_exact_target(target);
    let send = |key: &str| crate::proc::tmux_ok(&["send-keys", "-t", &target, key]);
    send(msg) && send("Enter")
}

/// `bn reviews --fetch` — rebuild the review-queue cache from GitHub: PRs awaiting your review and
/// your own open PRs, each stamped with state + reviewDecision. Replaces the gh+jq pipeline.
pub(crate) fn fetch_review_queue() -> i32 {
    if !gh_ready() {
        eprintln!("reviews: gh not found or not authenticated");
        return 0; // non-fatal, like the shim
    }
    let mut prs = Vec::new();
    for (query, tag) in [
        ("--review-requested=@me", "review"),
        ("--author=@me", "mine"),
    ] {
        prs.extend(gh_search_bucket(query, tag));
    }
    if prs.is_empty() {
        println!("reviews: nothing to review and no open PRs of mine — leaving cache as-is");
        return 0;
    }
    let cache = bn_core::reviews::ReviewCache {
        generated_at: bn_core::util::now_rfc3339_utc(),
        prs,
    };
    let path = bn_core::reviews::cache_path();
    let json = match serde_json::to_string_pretty(&cache) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("reviews: {e}");
            return 1;
        }
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&path, json) {
        Ok(()) => {
            println!(
                "reviews: wrote {} PR(s) to {}",
                cache.prs.len(),
                path.display()
            );
            0
        }
        Err(e) => {
            eprintln!("reviews: {e}");
            1
        }
    }
}

/// Trailing window `pr-observe` sweeps: PRs updated within this many days get (re)checked.
/// 30 matches the metrics scorecard window, so a fresh install backfills what the card shows.
pub(crate) const PR_OBSERVE_WINDOW_DAYS: i64 = 30;

/// `pr-observe` — fold the GitHub state of your own PRs into the metrics log as
/// `pr_opened`/`pr_merged`/`pr_closed`/`pr_review` events (the producer `bn metrics` was
/// missing, #213). One `gh search prs --author=@me` for the window, then one `gh pr view`
/// only for PRs whose terminal/review events are still unknown. Idempotent by
/// `(kind, repo, number)` against the whole-person log; events are stamped at their GitHub
/// timestamps, so the first run backfills the window truthfully.
pub(crate) fn job_pr_observe(cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    use super::pr_observe_gh::{gh_enrich_pr, gh_my_prs};
    use bn_core::pr_observe as po;
    if !gh_ready() {
        eprintln!("pr-observe: gh not found or not authenticated");
        return 0; // non-fatal, like the review-queue fetch
    }
    let seen = po::seen_keys(&bn_core::events::read_events_across(
        &cfg.metrics_logs_all_machines(),
    ));
    let since = bn_core::util::add_days(&today(), -PR_OBSERVE_WINDOW_DAYS).unwrap_or_else(today);
    let rows = gh_my_prs(&since);
    if rows.is_empty() {
        println!("pr-observe: no PRs of mine updated since {since}");
        return 0;
    }
    let mut prs = Vec::new();
    for row in rows {
        // Every repo is observed, including the scorecard-excluded ones. The exclusion is a
        // *reporting* rule, not a recording one: `metrics::visible_events` drops excluded repos at
        // read time for every metrics surface, so the scorecard stays unflattered while surfaces
        // that ask "what did I actually finish yesterday" (the standup fold) can still see the
        // work. Skipping here instead erased it from both, which is why a day spent entirely in
        // `bn` read back as "no completed tasks recorded".
        let mut pr = po::ObservedPr {
            repo: row.repo,
            number: row.number,
            title: row.title,
            created_ts: bn_core::util::parse_ts(&row.created_at),
            ..Default::default()
        };
        if po::needs_enrichment(&pr.repo, pr.number, row.closed, &seen) {
            gh_enrich_pr(&mut pr, &row.url);
        }
        prs.push(pr);
    }
    let pending = po::derive_events(&prs, &seen);
    if pending.is_empty() {
        println!("pr-observe: metrics log already up to date");
        return 0;
    }
    let n = pending.len();
    for ev in pending {
        if let Err(e) =
            bn_core::events::append_metric_at(&cfg.metrics_log_path(), ev.kind, ev.ts, ev.fields)
        {
            eprintln!("pr-observe: {e}");
            return 1;
        }
    }
    println!("pr-observe: appended {n} PR event(s)");
    0
}


/// `gh` present and authenticated.
pub(crate) fn gh_ready() -> bool {
    crate::gh::gh_ok(&["auth", "status"])
}

/// One `gh search prs <query>` bucket → `ReviewPr`s, each enriched with its state + reviewDecision
/// via `gh pr view` (matching the shim's per-PR verdict lookup).
pub(crate) fn gh_search_bucket(query: &str, tag: &str) -> Vec<bn_core::reviews::ReviewPr> {
    #[derive(serde::Deserialize)]
    struct SearchPr {
        repository: Repo,
        number: i64,
        #[serde(default)]
        title: String,
        author: Option<Author>,
        url: String,
        #[serde(default, rename = "createdAt")]
        created_at: String,
        #[serde(default, rename = "updatedAt")]
        updated_at: String,
    }
    #[derive(serde::Deserialize)]
    struct Repo {
        #[serde(rename = "nameWithOwner")]
        name_with_owner: String,
    }
    #[derive(serde::Deserialize, Default)]
    struct Author {
        #[serde(default)]
        login: String,
    }
    bn_core::runner::gh_json::<Vec<SearchPr>>(
        &bn_core::runner::RealRunner,
        &[
            "search",
            "prs",
            query,
            "--state=open",
            "--limit",
            "50",
            "--json",
            "repository,number,title,author,url,createdAt,updatedAt",
        ],
    )
    .unwrap_or_default()
    .into_iter()
    .map(|pr| {
        let (state, review_decision) = gh_verdict(&pr.url);
        bn_core::reviews::ReviewPr {
            repo: pr.repository.name_with_owner,
            number: pr.number,
            title: pr.title,
            author: pr.author.unwrap_or_default().login,
            url: pr.url,
            updated_at: pr.updated_at,
            created_at: pr.created_at,
            tag: tag.to_string(),
            state,
            review_decision,
        }
    })
    .collect()
}

/// A PR's `(state, reviewDecision)` from `gh pr view`; state lowercased and defaulted to `open`.
pub(crate) fn gh_verdict(url: &str) -> (String, Option<String>) {
    #[derive(serde::Deserialize, Default)]
    struct VerdictView {
        state: Option<String>,
        #[serde(rename = "reviewDecision")]
        review_decision: Option<String>,
    }
    let v = bn_core::runner::gh_json::<VerdictView>(
        &bn_core::runner::RealRunner,
        &["pr", "view", url, "--json", "state,reviewDecision"],
    )
    .unwrap_or_default();
    let state = v
        .state
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .unwrap_or_else(|| "open".to_string());
    let rd = v.review_decision.filter(|s| !s.is_empty());
    (state, rd)
}

/// Read an `i64` env var, falling back to `default` when unset or unparseable.
pub(crate) fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Max concurrent unattended `auto` agents the autonomous loop may run at once — the
/// `attention-auto-trigger` concurrency cap, from `BN_ATTN_AUTO_MAX` (default 1 → one at a time;
/// raise it to fan an overnight queue out across several agents). Clamped to at least 1.
pub(crate) fn attn_auto_max() -> usize {
    env_i64("BN_ATTN_AUTO_MAX", 1).max(1) as usize
}

#[cfg(test)]
mod commit_notes_upstream_tests {
    use super::*;

    fn git_ok(dir: &std::path::Path, args: &[&str]) -> bool {
        crate::git::git_command(args)
            .current_dir(dir)
            .status()
            .expect("run git")
            .success()
    }

    fn git_out(dir: &std::path::Path, args: &[&str]) -> String {
        let output = crate::git::git_command(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("utf8 git output")
            .trim()
            .to_string()
    }

    #[test]
    fn deleted_and_pruned_upstream_is_recreated() {
        let _env = bn_core::test_support::env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let remote = temp.path().join("remote.git");
        let notes = temp.path().join("notes");
        let remote_s = remote.to_string_lossy();
        let notes_s = notes.to_string_lossy();

        assert!(git_ok(
            temp.path(),
            &["init", "--quiet", "--bare", remote_s.as_ref()]
        ));
        assert!(git_ok(
            temp.path(),
            &["init", "--quiet", "-b", "topic", notes_s.as_ref()]
        ));
        assert!(git_ok(&notes, &["config", "user.name", "bn test"]));
        assert!(git_ok(
            &notes,
            &["config", "user.email", "bn-test@example.invalid"]
        ));
        std::fs::write(notes.join("note.md"), "# Notes\n\nfirst\n").expect("seed note");
        assert!(git_ok(&notes, &["add", "note.md"]));
        assert!(git_ok(&notes, &["commit", "--quiet", "-m", "seed"]));
        assert!(git_ok(
            &notes,
            &["remote", "add", "origin", remote_s.as_ref()]
        ));
        assert!(git_ok(&notes, &["push", "--quiet", "-u", "origin", "topic"]));

        assert!(git_ok(
            &notes,
            &["push", "--quiet", "origin", "--delete", "topic"]
        ));
        assert!(git_ok(&notes, &["fetch", "--quiet", "--prune", "origin"]));
        assert!(
            !git_ok(&notes, &["rev-parse", "--verify", "@{u}"]),
            "the remote-tracking ref must be pruned for this regression"
        );

        std::fs::write(notes.join("note.md"), "# Notes\n\nsecond\n").expect("change note");
        let cfg = Config {
            personal_root: notes.clone(),
            work_root: temp.path().join("missing"),
            hostname: "test-host".to_string(),
            ..Default::default()
        };

        assert_eq!(job_commit_notes(&cfg, None), 0);
        assert_eq!(
            git_out(&notes, &["rev-parse", "HEAD"]),
            git_out(
                temp.path(),
                &[
                    "--git-dir",
                    remote_s.as_ref(),
                    "rev-parse",
                    "refs/heads/topic"
                ]
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn push_failure_after_local_commit_is_nonfatal() {
        use std::os::unix::fs::PermissionsExt;

        let _env = bn_core::test_support::env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let remote = temp.path().join("remote.git");
        let notes = temp.path().join("notes");
        let hooks = temp.path().join("hooks");
        let remote_s = remote.to_string_lossy();
        let notes_s = notes.to_string_lossy();
        let hooks_s = hooks.to_string_lossy();

        assert!(git_ok(
            temp.path(),
            &["init", "--quiet", "--bare", remote_s.as_ref()]
        ));
        assert!(git_ok(
            temp.path(),
            &["init", "--quiet", "-b", "topic", notes_s.as_ref()]
        ));
        assert!(git_ok(&notes, &["config", "user.name", "bn test"]));
        assert!(git_ok(
            &notes,
            &["config", "user.email", "bn-test@example.invalid"]
        ));
        std::fs::write(notes.join("note.md"), "# Notes\n\nfirst\n").expect("seed note");
        assert!(git_ok(&notes, &["add", "note.md"]));
        assert!(git_ok(&notes, &["commit", "--quiet", "-m", "seed"]));
        assert!(git_ok(
            &notes,
            &["remote", "add", "origin", remote_s.as_ref()]
        ));
        assert!(git_ok(&notes, &["push", "--quiet", "-u", "origin", "topic"]));

        assert!(git_ok(
            &notes,
            &["push", "--quiet", "origin", "--delete", "topic"]
        ));
        assert!(git_ok(&notes, &["fetch", "--quiet", "--prune", "origin"]));
        assert!(
            !git_ok(&notes, &["rev-parse", "--verify", "@{u}"]),
            "the remote-tracking ref must be pruned for this regression"
        );

        std::fs::create_dir_all(&hooks).expect("hooks dir");
        let hook = hooks.join("pre-push");
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").expect("write hook");
        let mut perms = std::fs::metadata(&hook).expect("hook metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&hook, perms).expect("hook executable");
        assert!(git_ok(
            &notes,
            &["config", "core.hooksPath", hooks_s.as_ref()]
        ));

        std::fs::write(notes.join("note.md"), "# Notes\n\nsecond\n").expect("change note");
        let before = git_out(&notes, &["rev-parse", "HEAD"]);
        let cfg = Config {
            personal_root: notes.clone(),
            work_root: temp.path().join("missing"),
            hostname: "test-host".to_string(),
            ..Default::default()
        };

        assert_eq!(job_commit_notes(&cfg, None), 0);
        assert_ne!(git_out(&notes, &["rev-parse", "HEAD"]), before);
    }
}
