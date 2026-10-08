//! `commit-notes` — the job that checkpoints the branch-notes repo: decide whether there is
//! anything worth committing, commit it, and push when the branch tracks a remote.
//!
//! Split out of [`super`] under the never-grow file-size ratchet. It is the largest single job in
//! the registry and, unlike its neighbours, is mostly *pure* decision logic — what counts as
//! machine state churn, when a note is empty enough to delete, what the committer should do given
//! a git state — which is why it carries its own test module and why it moves as one piece.
use crate::*;

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
/// then pushes. Never blocks on credentials. Returns non-zero if any repo's commit or fetch
/// failed; a push miss or rebase conflict after a successful local commit is logged but left
/// non-fatal so the durable commit still counts as a green cron run.
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

    #[test]
    fn rebase_failure_after_local_commit_is_nonfatal() {
        let _env = bn_core::test_support::env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let remote = temp.path().join("remote.git");
        let notes = temp.path().join("notes");
        let other = temp.path().join("other");
        let remote_s = remote.to_string_lossy();
        let notes_s = notes.to_string_lossy();
        let other_s = other.to_string_lossy();

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
        std::fs::write(notes.join("note.md"), "# Notes\n\nshared\n").expect("seed note");
        assert!(git_ok(&notes, &["add", "note.md"]));
        assert!(git_ok(&notes, &["commit", "--quiet", "-m", "seed"]));
        assert!(git_ok(
            &notes,
            &["remote", "add", "origin", remote_s.as_ref()]
        ));
        assert!(git_ok(&notes, &["push", "--quiet", "-u", "origin", "topic"]));

        assert!(git_ok(
            temp.path(),
            &["clone", "--quiet", "-b", "topic", remote_s.as_ref(), other_s.as_ref()]
        ));
        assert!(git_ok(&other, &["config", "user.name", "bn test"]));
        assert!(git_ok(
            &other,
            &["config", "user.email", "bn-test@example.invalid"]
        ));
        std::fs::write(other.join("note.md"), "# Notes\n\nremote\n").expect("remote note");
        assert!(git_ok(&other, &["commit", "--quiet", "-am", "remote change"]));
        assert!(git_ok(&other, &["push", "--quiet", "origin", "topic"]));

        let before = git_out(&notes, &["rev-parse", "HEAD"]);
        std::fs::write(notes.join("note.md"), "# Notes\n\nlocal\n").expect("local note");
        let cfg = Config {
            personal_root: notes.clone(),
            work_root: temp.path().join("missing"),
            hostname: "test-host".to_string(),
            ..Default::default()
        };

        assert_eq!(job_commit_notes(&cfg, None), 0);
        assert_ne!(git_out(&notes, &["rev-parse", "HEAD"]), before);
        assert!(
            git_out(&notes, &["log", "-1", "--pretty=%s"]).starts_with("notes: auto-checkpoint ")
        );
    }
}
