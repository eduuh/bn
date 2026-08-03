//! Pre-dispatch decomposition of oversized GitHub issues into conflict-scoped children (#755).

use crate::*;
use bn_core::backlog::{self, AddSpec, BacklogKind, Substrate};
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
pub(super) fn orchestrate_decompose(
    cfg: &Config,
    ctx: Option<&Resolved>,
    repo_override: Option<&str>,
    issue: &str,
    raw_parts: &[String],
    priority: i64,
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

    let path = backlog::backlog_path();
    let now = bn_core::util::now_epoch_secs();
    let source = format!("decomposed:{slug}#{}", parent.number);
    for child in &children {
        let spec = AddSpec {
            kind: BacklogKind::Ship,
            priority,
            touches: child.touches.clone(),
            substrate: Substrate::Auto,
            source: source.clone(),
            controller_id: std::env::var("BN_CONTROLLER_ID").ok(),
            task: crate::cmd::task::task_for_enqueue(
                cfg,
                &repo,
                &child.title,
                &source,
            ),
        };
        if let Err(error) = backlog::add(&path, &repo, &[child.number.to_string()], &spec, now) {
            return fail("backlog-write", error.to_string());
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
                }),
            )
            .to_json()
        );
    } else {
        println!(
            "orchestrate: decomposed #{} into {} child issues; enqueued children only",
            parent.number,
            children.len()
        );
        for child in &children {
            let status = if child.reused { "reused" } else { "created" };
            println!(
                "  #{} {} ({status})  {{{}}}",
                child.number,
                child.title,
                child.touches.join(", ")
            );
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use bn_core::runner::FakeRunner;

    fn parent() -> DecompositionParent {
        DecompositionParent {
            number: 755,
            title: "Oversized bundle".to_string(),
            url: "https://github.com/eduuh/bn/issues/755".to_string(),
            labels: vec![DecompositionLabel {
                name: "enhancement".to_string(),
            }],
        }
    }

    #[test]
    fn parts_require_distinct_non_overlapping_scopes() {
        let valid = parse_decomposition_parts(&[
            "Core model::crates/bn-core/src/decompose.rs".to_string(),
            "CLI wiring::crates/bn-cli/src/cmd/decompose.rs,skills/**".to_string(),
        ])
        .unwrap();
        assert_eq!(valid.len(), 2);
        assert_eq!(
            valid[1].touches,
            vec!["crates/bn-cli/src/cmd/decompose.rs", "skills/**"]
        );

        let overlap = parse_decomposition_parts(&[
            "CLI model::crates/bn-cli/src/**".to_string(),
            "CLI route::crates/bn-cli/src/cli.rs".to_string(),
        ])
        .unwrap_err();
        assert!(overlap.contains("overlapping touches-globs"));
        assert!(parse_decomposition_parts(&["Only one::skills/**".to_string()]).is_err());
    }

    #[test]
    fn child_body_links_parent_and_declares_conflict_scope() {
        let part = DecompositionPart {
            title: "Add decomposition command".to_string(),
            touches: vec!["crates/bn-cli/src/cmd/decompose.rs".to_string()],
        };
        let body = decomposition_body(&parent(), &part);
        assert!(body.contains("Part of #755"));
        assert!(body.contains("`crates/bn-cli/src/cmd/decompose.rs`"));
        assert!(body.contains(&decomposition_marker(755, &part)));
    }

    #[test]
    fn child_creation_reuses_marker_or_creates_labeled_issue() {
        let parent = parent();
        let part = DecompositionPart {
            title: "Add decomposition command".to_string(),
            touches: vec!["crates/bn-cli/src/cmd/decompose.rs".to_string()],
        };
        let existing = vec![ExistingDecompositionChild {
            number: 800,
            title: part.title.clone(),
            url: "https://github.com/eduuh/bn/issues/800".to_string(),
            body: decomposition_marker(parent.number, &part),
        }];
        let reused =
            create_decomposition_child(&FakeRunner::new(), "eduuh/bn", &parent, &part, &existing)
                .unwrap();
        assert_eq!(reused.number, 800);
        assert!(reused.reused);

        let args = issue_create_args("eduuh/bn", &parent, &part, true);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let runner = FakeRunner::new().on("gh", &refs, "https://github.com/eduuh/bn/issues/801\n");
        let created = create_decomposition_child(&runner, "eduuh/bn", &parent, &part, &[]).unwrap();
        assert_eq!(created.number, 801);
        assert!(!created.reused);
    }

    #[test]
    fn marker_uses_the_complete_part_specification() {
        let first = DecompositionPart {
            title: "One two three four five six first ending".to_string(),
            touches: vec!["skills/first.md".to_string()],
        };
        let second = DecompositionPart {
            title: "One two three four five six second ending".to_string(),
            touches: vec!["skills/second.md".to_string()],
        };
        assert_ne!(
            decomposition_marker(755, &first),
            decomposition_marker(755, &second)
        );
    }

    #[test]
    fn native_sub_issue_linking_skips_existing_and_attaches_missing_children() {
        let children = vec![
            DecompositionChild {
                number: 800,
                title: "Existing".to_string(),
                url: "https://github.com/eduuh/bn/issues/800".to_string(),
                touches: vec!["skills/**".to_string()],
                reused: true,
            },
            DecompositionChild {
                number: 801,
                title: "New".to_string(),
                url: "https://github.com/eduuh/bn/issues/801".to_string(),
                touches: vec!["crates/bn-cli/src/**".to_string()],
                reused: false,
            },
        ];
        let endpoint = "repos/eduuh/bn/issues/755/sub_issues?per_page=100";
        let runner = FakeRunner::new()
            .on("gh", &["api", endpoint], r#"[{"number":800}]"#)
            .on(
                "gh",
                &["api", "repos/eduuh/bn/issues/801", "--jq", ".id"],
                "4914000000",
            )
            .on(
                "gh",
                &[
                    "api",
                    endpoint,
                    "--method",
                    "POST",
                    "--field",
                    "sub_issue_id=4914000000",
                ],
                "{}",
            );
        link_decomposition_children(&runner, "eduuh/bn", 755, &children).unwrap();
    }
}
