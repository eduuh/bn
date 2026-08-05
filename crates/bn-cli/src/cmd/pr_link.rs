//! `bn pr`/`bn link` — attach a PR/issue URL to the note; open/copy URLs.
use crate::*;

/// `bn pr [url] [--copy|--show]` — record a PR, or resolve the stored/gh-discovered PR and
/// open/copy/show it. Ports cmd_pr (minus the duplicate ## Collaboration line).
pub(crate) fn emit_pr(
    cfg: &Config,
    ctx: &Resolved,
    url: Option<String>,
    copy: bool,
    show: bool,
    as_json: bool,
) {
    if let Some(u) = url {
        match commands::set_pr(cfg, ctx, &u, &today()) {
            Ok(o) => {
                append_inherited_worker_event(
                    cfg,
                    ctx,
                    bn_core::controller::ControllerWorkerEventType::PrOpened,
                    "PR opened",
                    Some(&o.pr),
                )
                .unwrap_or_else(|e| fail(&format!("bn pr: controller event: {e}")));
                if as_json {
                    println!(
                        "{}",
                        Envelope::success("pr", payload::pr_data(&o)).to_json()
                    );
                } else {
                    println!("PR linked: {}", o.pr);
                }
            }
            Err(e) => fail(&e.message()),
        }
        return;
    }
    // Resolve: stored PR, else `gh pr view`, auto-saving what gh finds (like the oracle).
    let mut pr = commands::read_pr(cfg, ctx);
    if pr.is_none() {
        if let Some(found) = gh_pr_url() {
            let _ = commands::set_pr(cfg, ctx, &found, &today());
            if !as_json {
                println!("Auto-linked: {found}");
            }
            pr = Some(found);
        }
    }
    match pr {
        Some(u) => url_action(&u, copy, show, "pr", as_json),
        None => {
            if as_json {
                println!(
                    "{}",
                    Envelope::failure("pr", "not_found", "No PR linked").to_json()
                );
            } else {
                println!("No PR linked for {}/{}", ctx.repo, ctx.branch);
                eprintln!("Usage: bn pr <url>");
            }
            std::process::exit(1);
        }
    }
}

/// `bn link [id]` — record a work item id on the note, or read back the stored id.
pub(crate) fn emit_link(cfg: &Config, ctx: &Resolved, id: Option<String>, as_json: bool) {
    if let Some(i) = id {
        match commands::set_work_item(cfg, ctx, &i, &today()) {
            Ok(o) => {
                if as_json {
                    println!(
                        "{}",
                        Envelope::success("link", payload::url_data(&o.work_item)).to_json()
                    );
                } else {
                    println!("Work item linked: {}", o.work_item);
                }
            }
            Err(e) => fail(&e.message()),
        }
        return;
    }
    let Some(wi) = commands::read_work_item(cfg, ctx) else {
        if as_json {
            println!(
                "{}",
                Envelope::failure("link", "not_found", "No work item linked").to_json()
            );
        } else {
            println!("No work item linked for {}/{}", ctx.repo, ctx.branch);
            eprintln!("Usage: bn link <id>");
        }
        std::process::exit(1);
    };
    if as_json {
        println!(
            "{}",
            Envelope::success("link", payload::url_data(&wi)).to_json()
        );
    } else {
        println!("Work item: {wi}");
    }
}

/// Open (default), copy, or show a resolved URL. `--json` prints the `{ "url" }` envelope and
/// never performs the side effect (a machine caller wants the value, not a browser tab).
pub(crate) fn url_action(url: &str, copy: bool, show: bool, command: &str, as_json: bool) {
    if as_json {
        println!(
            "{}",
            Envelope::success(command, payload::url_data(url)).to_json()
        );
        return;
    }
    if copy {
        if copy_to_clipboard(url) {
            println!("Copied: {url}");
        } else {
            println!("{url}");
            eprintln!("(no clipboard tool found)");
        }
    } else if show {
        println!("{url}");
    } else if open_url(url) {
        println!("Opened: {url}");
    } else {
        println!("{url}");
    }
}

/// The PR URL for the current branch via `gh pr view --json url -q .url`, or `None` if gh is
/// absent / there's no PR / it fails. Run from cwd so gh resolves the right repo+branch.
pub(crate) fn gh_pr_url() -> Option<String> {
    bn_core::runner::pr_url(&bn_core::runner::RealRunner)
}

/// Resolve a human title for a captured issue/PR URL. GitHub issues/PRs go through `gh`;
/// anything else (or gh missing/failing) falls back to a readable tail of the URL. Best-effort —
/// always returns something usable as item text.
pub(crate) fn capture_title(url: &str) -> String {
    if let Some((_, rest)) = url.split_once("github.com/") {
        let sep = if rest.contains("/pull/") {
            Some(("/pull/", "pr"))
        } else if rest.contains("/issues/") {
            Some(("/issues/", "issue"))
        } else {
            None
        };
        if let Some((marker, kind)) = sep {
            let owner_repo = rest.split(marker).next().unwrap_or("");
            let num: String = rest
                .rsplit('/')
                .next()
                .unwrap_or("")
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if !owner_repo.is_empty() && !num.is_empty() {
                if let Some(t) = gh_view_title(kind, owner_repo, &num) {
                    return t;
                }
            }
        }
    }
    // Fallback: a readable tail of the URL.
    let tail = url.rsplit('/').next().unwrap_or(url);
    if url.contains("/pull/") {
        format!("PR #{tail}")
    } else if url.contains("/issues/") {
        format!("issue #{tail}")
    } else {
        tail.to_string()
    }
}

/// `gh <issue|pr> view <num> --repo <owner/repo> --json title -q .title`, or None on any failure
/// (gh absent, not authed, no such item). Best-effort title lookup for `bn capture`.
pub(crate) fn gh_view_title(kind: &str, owner_repo: &str, num: &str) -> Option<String> {
    bn_core::runner::view_title(&bn_core::runner::RealRunner, kind, owner_repo, num)
}
