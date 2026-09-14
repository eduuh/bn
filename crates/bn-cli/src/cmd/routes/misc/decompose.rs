//! Pre-dispatch decomposition of oversized GitHub issues into conflict-scoped children (#755).

use crate::*;
use bn_core::runner::CommandRunner;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct DecompositionPart {
    title: String,
    touches: Vec<String>,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct DecompositionParent {
    number: u64,
    title: String,
    url: String,
    #[serde(default)]
    labels: Vec<DecompositionLabel>,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct DecompositionLabel {
    name: String,
}

#[derive(Debug, serde::Deserialize)]
struct ExistingDecompositionChild {
    number: u64,
    title: String,
    url: String,
    #[serde(default)]
    body: String,
}

#[derive(Debug, serde::Deserialize)]
struct NativeSubIssue {
    number: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct DecompositionChild {
    number: u64,
    title: String,
    url: String,
    touches: Vec<String>,
    reused: bool,
}

fn parse_decomposition_parts(raw: &[String]) -> Result<Vec<DecompositionPart>, String> {
    if raw.len() < 2 {
        return Err("an oversized issue must be split into at least two --part values".to_string());
    }
    let mut parts = Vec::with_capacity(raw.len());
    for value in raw {
        let Some((title, globs)) = value.split_once("::") else {
            return Err(format!(
                "invalid --part '{value}' (want TITLE::GLOB[,GLOB])"
            ));
        };
        let title = title.trim();
        if title.is_empty() {
            return Err("a decomposition part has an empty title".to_string());
        }
        let mut touches = Vec::new();
        for glob in globs
            .split(',')
            .map(str::trim)
            .filter(|glob| !glob.is_empty())
        {
            if !touches.iter().any(|seen| seen == glob) {
                touches.push(glob.to_string());
            }
        }
        if touches.is_empty() {
            return Err(format!("decomposition part '{title}' has no touches-glob"));
        }
        if parts
            .iter()
            .any(|part: &DecompositionPart| part.title.eq_ignore_ascii_case(title))
        {
            return Err(format!("duplicate decomposition part title '{title}'"));
        }
        parts.push(DecompositionPart {
            title: title.to_string(),
            touches,
        });
    }
    for left in 0..parts.len() {
        for right in (left + 1)..parts.len() {
            if bn_core::orchestrate::touches_conflict(&parts[left].touches, &parts[right].touches) {
                return Err(format!(
                    "parts '{}' and '{}' have overlapping touches-globs",
                    parts[left].title, parts[right].title
                ));
            }
        }
    }
    Ok(parts)
}

fn decomposition_marker(parent: u64, part: &DecompositionPart) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in part.title.bytes().chain(std::iter::once(0)).chain(
        part.touches
            .iter()
            .flat_map(|glob| glob.bytes().chain(std::iter::once(0))),
    ) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("<!-- bn-decomposition parent={parent} child={hash:016x} -->")
}

fn decomposition_body(parent: &DecompositionParent, part: &DecompositionPart) -> String {
    let touches = part
        .touches
        .iter()
        .map(|glob| format!("`{glob}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Part of #{} ({})\n\n## Scope\n{}\n\n## Acceptance criteria\n- [ ] The scoped change is independently shippable.\n- [ ] Repository verification passes.\n\n## Conflict scope (touches-glob)\n{}\n\n{}",
        parent.number,
        parent.url,
        part.title,
        touches,
        decomposition_marker(parent.number, part)
    )
}

fn parse_created_issue(stdout: &str) -> Option<(u64, String)> {
    let url = stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| line.starts_with("http://") || line.starts_with("https://"))?
        .to_string();
    let number = url.trim_end_matches('/').rsplit('/').next()?.parse().ok()?;
    Some((number, url))
}

fn issue_create_args(
    slug: &str,
    parent: &DecompositionParent,
    part: &DecompositionPart,
    with_labels: bool,
) -> Vec<String> {
    let mut args = vec![
        "issue".to_string(),
        "create".to_string(),
        "-R".to_string(),
        slug.to_string(),
        "--title".to_string(),
        part.title.clone(),
        "--body".to_string(),
        decomposition_body(parent, part),
    ];
    if with_labels {
        for label in &parent.labels {
            args.push("--label".to_string());
            args.push(label.name.clone());
        }
    }
    args
}

fn create_decomposition_child(
    runner: &dyn CommandRunner,
    slug: &str,
    parent: &DecompositionParent,
    part: &DecompositionPart,
    existing: &[ExistingDecompositionChild],
) -> Result<DecompositionChild, String> {
    let marker = decomposition_marker(parent.number, part);
    if let Some(issue) = existing.iter().find(|issue| issue.body.contains(&marker)) {
        return Ok(DecompositionChild {
            number: issue.number,
            title: issue.title.clone(),
            url: issue.url.clone(),
            touches: part.touches.clone(),
            reused: true,
        });
    }

    let args = issue_create_args(slug, parent, part, true);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut output = runner.gh(&refs);
    if !output.success
        && output.stderr.contains("label")
        && output.stderr.contains("not found")
        && !parent.labels.is_empty()
    {
        let args = issue_create_args(slug, parent, part, false);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        output = runner.gh(&refs);
    }
    if !output.success {
        return Err(if output.stderr.trim().is_empty() {
            format!("gh issue create failed for '{}'", part.title)
        } else {
            output.stderr.trim().to_string()
        });
    }
    let Some((number, url)) = parse_created_issue(&output.stdout) else {
        return Err(format!(
            "created child '{}' but could not parse its issue URL",
            part.title
        ));
    };
    Ok(DecompositionChild {
        number,
        title: part.title.clone(),
        url,
        touches: part.touches.clone(),
        reused: false,
    })
}

fn link_decomposition_children(
    runner: &dyn CommandRunner,
    slug: &str,
    parent: u64,
    children: &[DecompositionChild],
) -> Result<(), String> {
    let endpoint = format!("repos/{slug}/issues/{parent}/sub_issues?per_page=100");
    let output = runner.gh(&["api", &endpoint]);
    if !output.success {
        return Err(if output.stderr.trim().is_empty() {
            "could not read the parent's native sub-issues".to_string()
        } else {
            output.stderr.trim().to_string()
        });
    }
    let linked: Vec<NativeSubIssue> = serde_json::from_str(&output.stdout)
        .map_err(|error| format!("invalid native sub-issue response: {error}"))?;
    for child in children {
        if linked.iter().any(|issue| issue.number == child.number) {
            continue;
        }
        let child_endpoint = format!("repos/{slug}/issues/{}", child.number);
        let output = runner.gh(&["api", &child_endpoint, "--jq", ".id"]);
        let Some(database_id) = output.ok_stdout() else {
            return Err(format!(
                "could not resolve database id for child #{}",
                child.number
            ));
        };
        let field = format!("sub_issue_id={database_id}");
        let output = runner.gh(&["api", &endpoint, "--method", "POST", "--field", &field]);
        if !output.success {
            return Err(if output.stderr.trim().is_empty() {
                format!(
                    "could not attach child #{} to parent #{parent}",
                    child.number
                )
            } else {
                output.stderr.trim().to_string()
            });
        }
    }
    Ok(())
}

fn resolve_repo(
    cfg: &Config,
    ctx: Option<&Resolved>,
    repo_override: Option<&str>,
) -> Result<String, String> {
    match wt::resolve_bare_by_name(cfg, repo_override) {
        Ok(Some((repo, _))) => Ok(repo),
        Ok(None) => ctx
            .map(|resolved| resolved.repo.clone())
            .ok_or_else(|| "not in a repo — pass --repo <name> to target one".to_string()),
        Err(error) => Err(error),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn issue_decompose(
    cfg: &Config,
    ctx: Option<&Resolved>,
    repo_override: Option<&str>,
    issue: &str,
    raw_parts: &[String],
    dry_run: bool,
    json: bool,
) -> bool {
    let fail = |code: &str, message: String| {
        if json {
            println!(
                "{}",
                Envelope::failure("orchestrate", code, message).to_json()
            );
        } else {
            eprintln!("bn orchestrate decompose: {message}");
        }
        false
    };
    let parts = match parse_decomposition_parts(raw_parts) {
        Ok(parts) => parts,
        Err(error) => return fail("invalid-parts", error),
    };
    let repo = match resolve_repo(cfg, ctx, repo_override) {
        Ok(repo) => repo,
        Err(error) => return fail("no-repo", error),
    };
    let runner = bn_core::runner::RealRunner;
    let Some(slug) = runner
        .gh(&[
            "repo",
            "view",
            &repo,
            "--json",
            "nameWithOwner",
            "--jq",
            ".nameWithOwner",
        ])
        .ok_stdout()
    else {
        return fail(
            "repo-resolve",
            format!("could not resolve GitHub repo for '{repo}'"),
        );
    };
    let parent_output = runner.gh(&[
        "issue",
        "view",
        issue,
        "-R",
        &slug,
        "--json",
        "number,title,url,labels",
    ]);
    if !parent_output.success {
        return fail("parent-read", parent_output.stderr.trim().to_string());
    }
    let parent: DecompositionParent = match serde_json::from_str(&parent_output.stdout) {
        Ok(parent) => parent,
        Err(error) => {
            return fail(
                "parent-read",
                format!("invalid parent issue response: {error}"),
            )
        }
    };

    if dry_run {
        if json {
            println!(
                "{}",
                Envelope::success(
                    "orchestrate",
                    serde_json::json!({
                        "dry_run": true,
                        "repo": slug,
                        "parent": parent,
                        "parts": parts,
                    }),
                )
                .to_json()
            );
        } else {
            println!(
                "orchestrate: would split #{} {} into {} child issues",
                parent.number,
                parent.title,
                parts.len()
            );
            for part in &parts {
                println!("  {}  {{{}}}", part.title, part.touches.join(", "));
            }
        }
        return true;
    }

    let existing_output = runner.gh(&[
        "issue",
        "list",
        "-R",
        &slug,
        "--state",
        "all",
        "--limit",
        "1000",
        "--json",
        "number,title,url,body",
    ]);
    if !existing_output.success {
        return fail("children-read", existing_output.stderr.trim().to_string());
    }
    let existing: Vec<ExistingDecompositionChild> =
        match serde_json::from_str(&existing_output.stdout) {
            Ok(existing) => existing,
            Err(error) => {
                return fail(
                    "children-read",
                    format!("invalid existing issues response: {error}"),
                );
            }
        };
    let mut children = Vec::with_capacity(parts.len());
    for part in &parts {
        match create_decomposition_child(&runner, &slug, &parent, part, &existing) {
            Ok(child) => children.push(child),
            Err(error) => return fail("child-create", error),
        }
    }
    if let Err(error) = link_decomposition_children(&runner, &slug, parent.number, &children) {
        return fail("child-link", error);
    }

    let checklist = children
        .iter()
        .map(|child| format!("- [ ] #{} — {}", child.number, child.title))
        .collect::<Vec<_>>()
        .join("\n");
    let comment = format!(
        "Decomposed into independently shippable conflict scopes:\n\n{checklist}\n\nDispatch these child issues instead of the parent bundle."
    );
    let parent_number = parent.number.to_string();
    let comment_output = runner.gh(&[
        "issue",
        "comment",
        &parent_number,
        "-R",
        &slug,
        "--body",
        &comment,
    ]);
    let parent_comment_linked = comment_output.success;
    if !parent_comment_linked && !json {
        eprintln!(
            "bn orchestrate decompose: warning: children link to the parent, but its checklist comment failed: {}",
            comment_output.stderr.trim()
        );
    }

    // Start the children, never the parent. Each goes through `bn dispatch`, whose branch guard
    // refuses a child already running, so re-running a decomposition does not start a second lane.
    let source = format!("decomposed:{slug}#{}", parent.number);
    let mut started = Vec::new();
    let mut not_started = Vec::new();
    for child in &children {
        let number = child.number.to_string();
        let task = crate::cmd::task_mint::task_for_subject(cfg, &repo, &child.title, &source);
        match crate::cmd::dispatch::DispatchInvocation::new(&number, &repo, "auto")
            .task(Some(&task))
            .launch()
        {
            Ok(()) => started.push(child.number),
            Err(error) => not_started.push(serde_json::json!({
                "number": child.number,
                "error": error,
            })),
        }
    }

    if json {
        println!(
            "{}",
            Envelope::success(
                "orchestrate",
                serde_json::json!({
                    "repo": slug,
                    "parent": parent,
                    "children": children,
                    "parent_comment_linked": parent_comment_linked,
                    "started": started,
                    "not_started": not_started,
                }),
            )
            .to_json()
        );
    } else {
        println!(
            "orchestrate: decomposed #{} into {} child issues; started {} of them",
            parent.number,
            children.len(),
            started.len()
        );
        for child in &children {
            let status = match (child.reused, started.contains(&child.number)) {
                (true, true) => "reused, started",
                (false, true) => "created, started",
                (true, false) => "reused, not started",
                (false, false) => "created, not started",
            };
            println!(
                "  #{} {} ({status})  {{{}}}",
                child.number,
                child.title,
                child.touches.join(", ")
            );
        }
        for failure in &not_started {
            eprintln!(
                "bn orchestrate decompose: could not start #{}: {}",
                failure["number"],
                failure["error"].as_str().unwrap_or_default()
            );
        }
    }
    not_started.is_empty()
}


