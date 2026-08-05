//! `bn issue` — auto-file deduped, labeled bn issues on identified problems (#345 proposal 4).
//!
//! The orchestrator (or an agent) that trips over a bug / friction / blocker files it as a
//! labeled GitHub issue instead of leaning on a human to do it by hand — the way #340 and #343
//! were filed manually. Filing is **deduped**: before opening one we scan the repo's OPEN issues
//! and, when a title already matches, report that issue instead of creating a duplicate. So the
//! same problem, hit twice, never spawns two issues — auto-filing on every tick stays idempotent.

use crate::cli::IssueKindArg;
use crate::*;
use bn_core::runner::{gh_json, CommandRunner, RealRunner};

/// An open issue as `gh issue list --json number,title` returns it.
#[derive(serde::Deserialize)]
struct OpenIssue {
    number: u64,
    title: String,
}

/// Normalize a title for dedup: lowercased, whitespace collapsed to single spaces, and trailing
/// ASCII punctuation stripped. Two titles that normalize equal are treated as the same issue, so
/// trivial re-wordings ("watch wedges." vs "Watch  wedges") don't slip a duplicate past the guard.
pub(crate) fn normalize_title(t: &str) -> String {
    t.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
        .trim_end_matches(['.', '!', '?', ':', ';', ','])
        .trim()
        .to_string()
}

/// The canonical label a `--kind` seeds. `bug`/`blocker` reuse GitHub's conventional labels;
/// `friction` and `enhancement` map to the bn-standard ones.
pub(crate) fn kind_label(kind: IssueKindArg) -> &'static str {
    match kind {
        IssueKindArg::Bug => "bug",
        IssueKindArg::Friction => "friction",
        IssueKindArg::Blocker => "blocker",
        IssueKindArg::Enhancement => "enhancement",
    }
}

/// Every label to apply: the `--kind` label first, then each `--label` extra, deduped
/// case-insensitively while preserving first-seen order.
pub(crate) fn issue_labels(kind: IssueKindArg, extra: &[String]) -> Vec<String> {
    let mut out = vec![kind_label(kind).to_string()];
    for l in extra {
        let l = l.trim();
        if !l.is_empty() && !out.iter().any(|e| e.eq_ignore_ascii_case(l)) {
            out.push(l.to_string());
        }
    }
    out
}

/// The number of an OPEN issue whose title matches `title` (normalized), or `None`. Pure over the
/// already-fetched list so it's unit-testable without touching gh.
fn find_duplicate(title: &str, open: &[(u64, String)]) -> Option<u64> {
    let want = normalize_title(title);
    open.iter()
        .find(|(_, t)| normalize_title(t) == want)
        .map(|(n, _)| *n)
}

/// `gh issue list` args to read the repo's open issues for the dedup scan.
fn list_args(slug: Option<&str>) -> Vec<String> {
    let mut a = vec!["issue".to_string(), "list".to_string()];
    if let Some(s) = slug {
        a.push("-R".to_string());
        a.push(s.to_string());
    }
    a.extend(
        [
            "--state",
            "open",
            "--limit",
            "300",
            "--json",
            "number,title",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    a
}

/// `gh issue create` args for a new issue with `labels` applied.
fn create_args(
    slug: Option<&str>,
    title: &str,
    body: Option<&str>,
    labels: &[String],
) -> Vec<String> {
    let mut a = vec!["issue".to_string(), "create".to_string()];
    if let Some(s) = slug {
        a.push("-R".to_string());
        a.push(s.to_string());
    }
    a.push("--title".to_string());
    a.push(title.to_string());
    a.push("--body".to_string());
    a.push(body.unwrap_or("").to_string());
    for l in labels {
        a.push("--label".to_string());
        a.push(l.clone());
    }
    a
}

/// The `owner/name` slug: an explicit `owner/repo` override wins; else `gh repo view` reads it
/// from the cwd's origin. `None` when neither resolves (gh absent / not a repo).
fn resolve_slug(runner: &dyn CommandRunner, repo_override: Option<&str>) -> Option<String> {
    if let Some(r) = repo_override {
        if r.contains('/') {
            return Some(r.to_string());
        }
    }
    let mut args: Vec<&str> = vec!["repo", "view"];
    if let Some(r) = repo_override {
        args.push(r);
    }
    args.extend_from_slice(&["--json", "nameWithOwner", "-q", ".nameWithOwner"]);
    runner.gh(&args).ok_stdout()
}

/// The last `http(s)://…` line of `gh issue create`'s output — the URL it prints on success.
fn parse_issue_url(out: &str) -> Option<String> {
    out.lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with("http://") || l.starts_with("https://"))
        .map(str::to_string)
}

/// `bn issue file` — file a deduped, labeled issue (or report the existing match).
pub(crate) fn emit_issue_file(
    title: Vec<String>,
    body: Option<String>,
    kind: IssueKindArg,
    extra_labels: Vec<String>,
    repo: Option<String>,
    allow_duplicate: bool,
    as_json: bool,
) {
    let runner = RealRunner;
    let title = title.join(" ");
    let title = title.trim();
    if title.is_empty() {
        fail_json(as_json, "issue", "empty_title", "Issue title is empty");
    }
    let slug = resolve_slug(&runner, repo.as_deref());

    // Dedup: scan the repo's open issues; a title match short-circuits to reporting it.
    if !allow_duplicate {
        let open: Vec<(u64, String)> =
            gh_json::<Vec<OpenIssue>>(&runner, &str_args(&list_args(slug.as_deref())))
                .unwrap_or_default()
                .into_iter()
                .map(|i| (i.number, i.title))
                .collect();
        if let Some(num) = find_duplicate(title, &open) {
            let url = slug
                .as_deref()
                .map(|s| format!("https://github.com/{s}/issues/{num}"));
            if as_json {
                println!(
                    "{}",
                    Envelope::success(
                        "issue",
                        serde_json::json!({
                            "filed": false,
                            "deduped": true,
                            "number": num,
                            "url": url,
                            "title": title,
                        }),
                    )
                    .to_json()
                );
            } else {
                println!("Duplicate — open issue #{num} already tracks this:");
                if let Some(u) = &url {
                    println!("  {u}");
                }
                println!("  (file anyway with --allow-duplicate)");
            }
            return;
        }
    }

    let labels = issue_labels(kind, &extra_labels);
    let out = runner.gh(&str_args(&create_args(
        slug.as_deref(),
        title,
        body.as_deref(),
        &labels,
    )));
    // Auto-filing must not hard-fail just because a label is missing in the target repo — retry
    // once without labels so the issue still lands (with a warning), rather than dropping it.
    let (out, dropped_labels) = if !out.success
        && out.stderr.contains("label")
        && out.stderr.contains("not found")
        && !labels.is_empty()
    {
        (
            runner.gh(&str_args(&create_args(
                slug.as_deref(),
                title,
                body.as_deref(),
                &[],
            ))),
            true,
        )
    } else {
        (out, false)
    };

    if !out.success {
        let msg = out.stderr.trim();
        fail_json(
            as_json,
            "issue",
            "create_failed",
            if msg.is_empty() {
                "gh issue create failed (is gh installed and authed?)"
            } else {
                msg
            },
        );
    }

    let url = parse_issue_url(&out.stdout);
    let applied: Vec<String> = if dropped_labels { Vec::new() } else { labels };
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "issue",
                serde_json::json!({
                    "filed": true,
                    "deduped": false,
                    "url": url,
                    "title": title,
                    "labels": applied,
                    "labels_dropped": dropped_labels,
                }),
            )
            .to_json()
        );
    } else {
        match &url {
            Some(u) => println!("Filed issue: {u}"),
            None => println!("Filed issue (URL not captured)."),
        }
        if !applied.is_empty() {
            println!("  labels: {}", applied.join(", "));
        }
        if dropped_labels {
            println!("  note: labels not found in repo were dropped so the issue could be filed");
        }
    }
}

/// Borrow a `Vec<String>` as the `&[&str]` the runner seam wants.
fn str_args(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bn_core::runner::FakeRunner;

    #[test]
    fn normalize_collapses_case_space_and_trailing_punct() {
        assert_eq!(normalize_title("  Watch  Wedges. "), "watch wedges");
        assert_eq!(normalize_title("Watch wedges"), "watch wedges");
        assert_eq!(normalize_title("A!?"), "a");
    }

    #[test]
    fn labels_lead_with_kind_and_dedupe_extras() {
        assert_eq!(
            issue_labels(
                IssueKindArg::Bug,
                &["urgent".into(), "Bug".into(), "".into()]
            ),
            vec!["bug", "urgent"]
        );
        assert_eq!(
            issue_labels(IssueKindArg::Enhancement, &[]),
            vec!["enhancement"]
        );
    }

    #[test]
    fn find_duplicate_matches_normalized_title_only() {
        let open = vec![
            (10, "Something else".to_string()),
            (42, "Watch  wedges.".to_string()),
        ];
        assert_eq!(find_duplicate("watch wedges", &open), Some(42));
        assert_eq!(find_duplicate("Watch wedges!", &open), Some(42));
        assert_eq!(find_duplicate("brand new problem", &open), None);
    }

    #[test]
    fn list_and_create_args_include_repo_and_labels() {
        assert_eq!(
            list_args(Some("me/repo")),
            vec![
                "issue",
                "list",
                "-R",
                "me/repo",
                "--state",
                "open",
                "--limit",
                "300",
                "--json",
                "number,title"
            ]
        );
        assert_eq!(
            create_args(
                Some("me/repo"),
                "T",
                Some("B"),
                &["bug".into(), "urgent".into()]
            ),
            vec![
                "issue", "create", "-R", "me/repo", "--title", "T", "--body", "B", "--label",
                "bug", "--label", "urgent"
            ]
        );
        // No slug, no body → empty --body, no -R.
        assert_eq!(
            create_args(None, "T", None, &[]),
            vec!["issue", "create", "--title", "T", "--body", ""]
        );
    }

    #[test]
    fn parse_issue_url_takes_last_http_line() {
        let out = "Creating issue in me/repo\nhttps://github.com/me/repo/issues/7\n";
        assert_eq!(
            parse_issue_url(out).as_deref(),
            Some("https://github.com/me/repo/issues/7")
        );
        assert_eq!(parse_issue_url("no url"), None);
    }

    #[test]
    fn resolve_slug_prefers_explicit_then_gh() {
        let r = FakeRunner::new();
        assert_eq!(
            resolve_slug(&r, Some("me/repo")).as_deref(),
            Some("me/repo")
        );
        let r = FakeRunner::new().on(
            "gh",
            &[
                "repo",
                "view",
                "--json",
                "nameWithOwner",
                "-q",
                ".nameWithOwner",
            ],
            "you/thing",
        );
        assert_eq!(resolve_slug(&r, None).as_deref(), Some("you/thing"));
    }
}
