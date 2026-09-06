//! Legacy remote-run tracking helpers.
//!
//! Remote/GitHub-hosted dispatch is no longer a supported launch path. This module remains only to
//! read and reconcile historical `remote-run:` note records so status/fleet views can render old
//! runs without losing their terminal state. The `gh` probes go through the injectable
//! [`CommandRunner`] seam so the behavior is unit-testable with a [`FakeRunner`] — no network, no
//! real `gh`.

use crate::run_status::{classify_run, RunStatus};
use crate::runner::CommandRunner;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Historical repo-relative path of the old dispatch workflow.
pub const WORKFLOW_REL_PATH: &str = ".github/workflows/claude-dispatch.yml";

/// Historical API-key secret used by the old workflow.
pub const SECRET_NAME: &str = "ANTHROPIC_API_KEY";

/// A repo's historical remote-run wiring state: is the old workflow present, and is the API-key
/// secret set?
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RemoteReadiness {
    /// `.github/workflows/claude-dispatch.yml` exists in the repo.
    pub workflow_present: bool,
    /// The `ANTHROPIC_API_KEY` secret is set (seen in `gh secret list`).
    pub secret_set: bool,
    /// Whether the secret state is actually known. We only probe `gh secret list` when the
    /// workflow is present, and the probe can fail (gh missing / unauthed / repo unresolvable);
    /// `false` means "unknown", which is what keeps an *unconfigured* repo from looking broken.
    pub secret_checked: bool,
}

impl RemoteReadiness {
    /// Fully wired: workflow scaffolded and the secret is set.
    pub fn is_ready(&self) -> bool {
        self.workflow_present && self.secret_checked && self.secret_set
    }

    /// Configured at all — the old workflow is scaffolded. Kept for legacy status/doctor callers.
    pub fn is_configured(&self) -> bool {
        self.workflow_present
    }

    /// The one doctor-worthy state: the workflow is in place but the secret is confirmed missing.
    /// A dispatch here would fail auth, so it's worth a nudge — unlike an unconfigured repo, or one
    /// whose secret state we couldn't determine.
    pub fn is_misconfigured(&self) -> bool {
        self.workflow_present && self.secret_checked && !self.secret_set
    }
}

/// Does `.github/workflows/claude-dispatch.yml` exist under `repo_root`? Pure filesystem check,
/// no network — the cheap gate that decides whether the `gh` probe is even worth running.
pub fn workflow_present(repo_root: &Path) -> bool {
    repo_root.join(WORKFLOW_REL_PATH).is_file()
}

/// What `scaffold_workflow` did to the target file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaffoldOutcome {
    /// The workflow file did not exist and was written.
    Created,
    /// The file existed with different content and was overwritten.
    Updated,
    /// The file already matched `content` byte-for-byte — nothing written.
    Unchanged,
}

/// Write `content` to `<repo_root>/.github/workflows/claude-dispatch.yml`, creating parent
/// directories. Idempotent and drift-correcting: the workflow is bn-managed, so an existing file
/// with different content is overwritten (reported as `Updated`); an identical file is left as-is
/// (`Unchanged`); an absent file is `Created`.
pub fn scaffold_workflow(repo_root: &Path, content: &str) -> std::io::Result<ScaffoldOutcome> {
    let path = repo_root.join(WORKFLOW_REL_PATH);
    let outcome = match std::fs::read_to_string(&path) {
        Ok(existing) if existing == content => return Ok(ScaffoldOutcome::Unchanged),
        Ok(_) => ScaffoldOutcome::Updated,
        Err(_) => ScaffoldOutcome::Created,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, content)?;
    Ok(outcome)
}

/// Is `name` among the secrets in `gh secret list` output? `gh secret list` prints one secret per
/// line as `NAME<whitespace>UPDATED_AT` (no values — secrets are write-only), so we match the
/// first whitespace-delimited column. A header row or blank lines can't false-positive.
pub fn secret_in_list(output: &str, name: &str) -> bool {
    output
        .lines()
        .any(|line| line.split_whitespace().next() == Some(name))
}

/// Probe a repo's remote-dispatch readiness. Always does the filesystem workflow check; only when
/// the workflow is present does it run `gh secret list` (via `runner`, cwd-scoped so `gh`
/// auto-detects the repo). Skipping the probe for unconfigured repos keeps `bn doctor` cheap and
/// offline in the common case, and keeps `secret_checked = false` as the "unconfigured" signal.
pub fn readiness(runner: &dyn CommandRunner, repo_root: &Path) -> RemoteReadiness {
    if !workflow_present(repo_root) {
        return RemoteReadiness {
            workflow_present: false,
            secret_set: false,
            secret_checked: false,
        };
    }
    let out = runner.gh(&["secret", "list"]);
    if !out.success {
        // gh missing / unauthed / repo unresolvable — configured, but state unknown.
        return RemoteReadiness {
            workflow_present: true,
            secret_set: false,
            secret_checked: false,
        };
    }
    RemoteReadiness {
        workflow_present: true,
        secret_set: secret_in_list(&out.stdout, SECRET_NAME),
        secret_checked: true,
    }
}

// ── Remote-run tracking (#282) ─────────────────────────────────────────────────────────────────
//
// Older builds recorded triggered GitHub Actions runs in the branch note. This half tracks those
// legacy records: classify the run's outcome onto the #271 run-status taxonomy and reconcile a
// recorded run against live `gh` (its state + the draft PR it opened) so `bn status` / `bn agent live`
// surface a historical remote run alongside local tmux agents. The failure→taxonomy table is the #279 spike's
// (branch note `remote-gh-spike.md`); all `gh` access routes through the [`CommandRunner`] seam.

/// A GitHub Actions run as `gh run view --json status,conclusion` reports it. `status` is the run
/// lifecycle (`queued` / `in_progress` / `completed` / …); `conclusion` is `null` until the run
/// completes, then one of GitHub's terminal conclusions (`success` / `failure` / `cancelled` / …).
#[derive(Debug, Clone, Deserialize)]
pub struct GhRunView {
    pub status: String,
    #[serde(default)]
    pub conclusion: Option<String>,
}

/// A pull request as `gh pr list --json number,url,state,isDraft` reports it. `state` is GitHub's
/// uppercase `OPEN` / `CLOSED` / `MERGED`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrRef {
    pub number: u64,
    pub url: String,
    pub state: String,
    #[serde(rename = "isDraft", default)]
    pub is_draft: bool,
}

impl PrRef {
    /// The PR is still open (the "PR link when open" surfacing case).
    pub fn is_open(&self) -> bool {
        self.state.eq_ignore_ascii_case("open")
    }

    /// The PR has been merged — the remote run's work landed.
    pub fn is_merged(&self) -> bool {
        self.state.eq_ignore_ascii_case("merged")
    }
}

/// The live state of a remote run for bn's status surfaces: still running, gone (`gh` can no longer
/// resolve it), or finished and mapped onto the #271 taxonomy ([`RunStatus`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteRunState {
    /// Not finished yet (`gh run view` status is anything but `completed`).
    Running,
    /// `gh` couldn't resolve the run (deleted, or its logs aged out) — reported distinctly rather
    /// than as a spurious failure, so a vanished run never pages anyone.
    Vanished,
    /// Finished, mapped onto the #271 taxonomy.
    Done(RunStatus),
}

impl RemoteRunState {
    /// A stable one-word label for panels and JSON: `running` | `gone` | the taxonomy word.
    pub fn label(self) -> &'static str {
        match self {
            RemoteRunState::Running => "running",
            RemoteRunState::Vanished => "gone",
            RemoteRunState::Done(s) => s.as_str(),
        }
    }

    /// The terminal taxonomy status, if the run has finished (`None` while running / vanished).
    pub fn run_status(self) -> Option<RunStatus> {
        match self {
            RemoteRunState::Done(s) => Some(s),
            _ => None,
        }
    }

    /// Whether the run has reached a terminal state — the signal that bn can stop polling it and
    /// persist the outcome into the note.
    pub fn is_terminal(self) -> bool {
        matches!(self, RemoteRunState::Done(_))
    }
}

/// Serialize as the one-word [`label`](RemoteRunState::label) so the fleet / status JSON carries a
/// flat `"state": "ok"` (etc.), not a Rust enum shape.
impl Serialize for RemoteRunState {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.label())
    }
}

/// Map a completed run's GitHub `conclusion` (plus, for a red run, its captured log) onto the #271
/// taxonomy. The log is consulted only for `failure` / `startup_failure`, where [`classify_run`]
/// separates a dead login (`auth`) or a hit usage window (`rate-limited`) from a genuine `fail`;
/// every other conclusion maps by name alone. An unknown conclusion is an honest `fail` — a
/// completed run bn can't vouch for. Table: #279 spike (`remote-gh-spike.md`).
pub fn gh_conclusion_status(conclusion: &str, log: &str) -> RunStatus {
    match conclusion {
        "success" => RunStatus::Ok,
        // A gate / no-op result: the run reached a conclusion without doing (or being allowed to
        // do) the work. Not a breakage to page on.
        "skipped" | "neutral" | "stale" | "action_required" => RunStatus::Skipped,
        // A workflow timeout surfaces as `cancelled` or `timed_out`; either is a real breakage.
        "cancelled" | "timed_out" => RunStatus::Fail,
        // A red run: let the failed-step log separate auth / rate-limited from a plain fail (#271).
        "failure" | "startup_failure" => classify_run(false, log),
        _ => RunStatus::Fail,
    }
}

/// Classify a run from its `gh run view` lifecycle. A run that has not `completed` is
/// [`RemoteRunState::Running`]; a completed run maps its `conclusion` via [`gh_conclusion_status`].
/// A completed run somehow missing a conclusion is treated defensively as a `fail`.
pub fn classify_gh_run(status: &str, conclusion: Option<&str>, log: &str) -> RemoteRunState {
    if status != "completed" {
        return RemoteRunState::Running;
    }
    match conclusion {
        Some(c) => RemoteRunState::Done(gh_conclusion_status(c, log)),
        None => RemoteRunState::Done(RunStatus::Fail),
    }
}

/// `gh run view` args for a run's lifecycle (`--json status,conclusion`). Kept a pure builder so
/// the reconcile's gh call and its test agree on the exact invocation. `--json` and `--log` are
/// mutually exclusive in `gh`, so the failed-step log is a separate call ([`run_log_args`]).
pub fn run_view_args(slug: &str, run_id: u64) -> Vec<String> {
    vec![
        "run".into(),
        "view".into(),
        run_id.to_string(),
        "-R".into(),
        slug.into(),
        "--json".into(),
        "status,conclusion".into(),
    ]
}

/// `gh run view --log-failed` args: only the failed steps' logs, which is all [`classify_run`]
/// needs to spot an auth / rate-limit marker — and far smaller than the whole `--log`.
pub fn run_log_args(slug: &str, run_id: u64) -> Vec<String> {
    vec![
        "run".into(),
        "view".into(),
        run_id.to_string(),
        "-R".into(),
        slug.into(),
        "--log-failed".into(),
    ]
}

/// `gh pr list` args to enumerate a repo's recent PRs (newest first) with their bodies.
/// `claude-dispatch.yml` writes the triggering run's URL into the PR body ("Triggering run:
/// <url>"), so [`find_run_pr`] correlates run → PR by scanning those bodies **locally** — an exact,
/// non-indexed match that (unlike `--search`) has no code-search index lag, so a just-opened PR is
/// found immediately. `--state all` so a merged / closed PR still resolves. Capped at
/// [`PR_LIST_LIMIT`]: a dispatch PR is opened at the end of the run it correlates to, so it's among
/// the newest — and a *saturated* result (no match, list full) is treated as inconclusive rather
/// than a false "no PR", so the cap never causes a wrong classification.
pub fn pr_list_args(slug: &str) -> Vec<String> {
    vec![
        "pr".into(),
        "list".into(),
        "-R".into(),
        slug.into(),
        "--state".into(),
        "all".into(),
        "--json".into(),
        "number,url,state,isDraft,body".into(),
        "-L".into(),
        PR_LIST_LIMIT.to_string(),
    ]
}

/// How many recent PRs [`find_run_pr`] scans. Also the saturation threshold: if the list comes back
/// full with no match, the window may have truncated an older matching PR, so the lookup is
/// `Unavailable` (retry) rather than `Absent`.
const PR_LIST_LIMIT: usize = 100;

/// The PR-body line `claude-dispatch.yml` writes to link a draft PR back to its triggering run.
const RUN_LINK_MARKER: &str = "Triggering run: ";

/// Does `body` link back to `run_url` via the dispatch workflow's `Triggering run: <url>` line?
/// Scans only the bn-authored metadata section (before the `---` divider) — never the untrusted
/// prompt that follows, which may quote a run URL — and requires an exact whole-line match, so one
/// run id can't prefix-match another (`…/runs/12` vs `…/runs/123`).
fn body_links_run(body: &str, run_url: &str) -> bool {
    let marker_line = format!("{RUN_LINK_MARKER}{run_url}");
    let head = body.split("\n---").next().unwrap_or(body);
    head.lines().any(|l| l.trim() == marker_line)
}

/// The Actions run URL for a `(slug, run_id)` — the correlation key the dispatch workflow echoes
/// into the PR body, and how [`reconcile_remote_run`] finds the linked PR.
pub fn run_url(slug: &str, run_id: u64) -> String {
    format!("https://github.com/{slug}/actions/runs/{run_id}")
}

/// `owner/repo` from a GitHub remote URL, or `None` for a non-GitHub / malformed remote. Pure
/// (testable): handles `git@github.com:owner/repo.git` and `https://github.com/owner/repo(.git)`.
/// Mirrors the CLI's `wt::remote::slug_from_url`, hoisted to bn-core so the web layer (#556 repo
/// issues) can resolve a bare clone's slug without depending on bn-cli.
pub fn github_slug_from_url(url: &str) -> Option<String> {
    let after = url.split("github.com").nth(1)?;
    let after = after.trim_start_matches([':', '/']);
    let slug = after.trim_end_matches(".git").trim_end_matches('/');
    let mut parts = slug.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if owner.is_empty() || repo.is_empty() {
        None
    } else {
        Some(format!("{owner}/{repo}"))
    }
}

/// The `owner/repo` GitHub slug of a bare clone at `git_dir`, via `git --git-dir <dir> remote
/// get-url origin`, or `None` when the remote is missing / non-GitHub / git errored. Run through
/// `runner` so it's testable with [`FakeRunner`].
pub fn github_slug(runner: &dyn CommandRunner, git_dir: &Path) -> Option<String> {
    let dir = git_dir.to_string_lossy();
    let out = runner.run(
        "git",
        &["--git-dir", dir.as_ref(), "remote", "get-url", "origin"],
    );
    if !out.success {
        return None;
    }
    github_slug_from_url(out.stdout.trim())
}

/// One row of `gh pr list --json …,body` — a candidate PR with its body, so [`find_run_pr`] can
/// correlate on the run URL the dispatch workflow writes into it.
#[derive(Debug, Clone, Deserialize)]
struct PrListRow {
    number: u64,
    url: String,
    state: String,
    #[serde(rename = "isDraft", default)]
    is_draft: bool,
    #[serde(default)]
    body: String,
}

/// The outcome of correlating a finished run to its draft PR — deliberately three-valued so the
/// caller can tell a genuine no-PR (the "green-but-no-PR" case → `skipped`) from a lookup that
/// couldn't be trusted (`gh` errored, or the PR window saturated), which must not be mistaken for
/// absence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrLookup {
    /// The PR the run opened, found by its run URL in the body.
    Found(PrRef),
    /// `gh` listed the repo's PRs (fewer than the cap, so exhaustive) and none references this run
    /// — the run opened no PR.
    Absent,
    /// The lookup was inconclusive: `gh` errored, or the list came back full so an older matching
    /// PR may have been truncated. Delivery is unknown, not absent.
    Unavailable,
}

/// Correlate a finished run to the draft PR it opened: list the repo's recent PRs and match the one
/// whose body embeds this run's URL (written there by `claude-dispatch.yml`). Local, exact matching
/// over [`pr_list_args`] — no code-search index lag. A match is `Found`; a short (non-saturated)
/// list with no match is a trustworthy `Absent`; a failed call or a saturated list is `Unavailable`
/// so the cap never yields a false "no PR".
pub fn find_run_pr(runner: &dyn CommandRunner, slug: &str, run_url: &str) -> PrLookup {
    let args = pr_list_args(slug);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let Some(rows) = crate::runner::gh_json::<Vec<PrListRow>>(runner, &refs) else {
        return PrLookup::Unavailable;
    };
    match rows.iter().find(|r| body_links_run(&r.body, run_url)) {
        Some(r) => PrLookup::Found(PrRef {
            number: r.number,
            url: r.url.clone(),
            state: r.state.clone(),
            is_draft: r.is_draft,
        }),
        // No match: only trust it as absence when the list wasn't truncated by the cap.
        None if rows.len() >= PR_LIST_LIMIT => PrLookup::Unavailable,
        None => PrLookup::Absent,
    }
}

/// One reconciled remote run: its live [`RemoteRunState`] and the linked PR when found. Built from
/// live `gh` by [`reconcile_remote_run`]; the shared record `bn status` / `bn agent live` render and (on
/// `bn status`) write back into the branch note once terminal.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RemoteRunReport {
    pub run_id: u64,
    pub slug: String,
    pub run_url: String,
    pub state: RemoteRunState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<PrRef>,
}

/// Reconcile a recorded remote run against live `gh`: read its lifecycle (`gh run view`), classify
/// it onto the #271 taxonomy (fetching the failed-step log only for a red run), and — once it has
/// finished — correlate the draft PR it opened. A run `gh` can no longer resolve reconciles to
/// [`RemoteRunState::Vanished`], never a spurious failure. All `gh` access routes through `runner`,
/// so the whole flow is unit-testable with a [`FakeRunner`].
///
/// **Green-but-no-PR is not delivery** (#279 finding): a `success` run that opened no PR did
/// nothing reviewable, so it's downgraded from `ok` to `skipped`. Because the workflow opens its PR
/// in the run's final step, by the time the conclusion is `success` the PR (if any) already exists
/// and lists, so an `Absent` lookup is a real no-PR. When the PR lookup is `Unavailable` (gh
/// errored) delivery is unknown, so the run's own `ok` stands rather than an invented `skipped`.
pub fn reconcile_remote_run(
    runner: &dyn CommandRunner,
    slug: &str,
    run_id: u64,
) -> RemoteRunReport {
    let run_url = run_url(slug, run_id);
    let view_args = run_view_args(slug, run_id);
    let view_refs: Vec<&str> = view_args.iter().map(String::as_str).collect();
    let Some(view) = crate::runner::gh_json::<GhRunView>(runner, &view_refs) else {
        // gh can't resolve the run — gone (deleted / logs aged out), not failed.
        return RemoteRunReport {
            run_id,
            slug: slug.to_string(),
            run_url,
            state: RemoteRunState::Vanished,
            pr: None,
        };
    };

    // Only a red run needs its log (to separate auth / rate-limited from fail); skip the fetch
    // otherwise, so a green or still-running run costs a single gh call.
    let needs_log = view.status == "completed"
        && matches!(
            view.conclusion.as_deref(),
            Some("failure") | Some("startup_failure")
        );
    let log = if needs_log {
        let log_args = run_log_args(slug, run_id);
        let log_refs: Vec<&str> = log_args.iter().map(String::as_str).collect();
        runner.gh(&log_refs).ok_stdout().unwrap_or_default()
    } else {
        String::new()
    };

    let mut state = classify_gh_run(&view.status, view.conclusion.as_deref(), &log);

    // The dispatch workflow opens its PR in a final step, so only correlate one once the run is
    // terminal — a still-running run has no PR to find yet.
    let pr = if state.is_terminal() {
        match find_run_pr(runner, slug, &run_url) {
            PrLookup::Found(pr) => Some(pr),
            // Green-but-no-PR ≠ ok: a successful run that delivered nothing is `skipped`.
            PrLookup::Absent => {
                if state == RemoteRunState::Done(RunStatus::Ok) {
                    state = RemoteRunState::Done(RunStatus::Skipped);
                }
                None
            }
            // Couldn't check delivery — leave the run's own outcome intact, attach no PR.
            PrLookup::Unavailable => None,
        }
    } else {
        None
    };

    RemoteRunReport {
        run_id,
        slug: slug.to_string(),
        run_url,
        state,
        pr,
    }
}
