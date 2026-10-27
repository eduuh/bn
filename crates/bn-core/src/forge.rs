//! Where a repo lives in a browser: its `origin` remote resolved to the forge's code, issue and
//! pull-request pages, so CLI consumers can use the link without reconstructing it.
//!
//! The remote is read straight out of the clone's `config` file rather than by shelling out to
//! `git remote get-url`, avoiding a process spawn per repo when deriving links for a whole fleet.
//!
//! Every URL here is *derived*, never probed — no network, so a link is only as right as the remote
//! is. Only forges whose URL layout is actually known produce issue / PR links; anything else gets
//! its code page and two `None`s, because a link that 404s is worse than an absent one.

use serde::Serialize;
use std::path::Path;

/// The forges whose web layout this module knows. `Unknown` still carries a code URL — the origin
/// parsed fine, it just isn't a host whose issue / PR paths we can name. Serialized names are
/// spelled out rather than derived: `rename_all` would turn `GitHub` into `git-hub`, and this string
/// is a wire contract the UI switches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Forge {
    #[serde(rename = "github")]
    GitHub,
    #[serde(rename = "azure-devops")]
    AzureDevOps,
    #[serde(rename = "gitlab")]
    GitLab,
    #[serde(rename = "unknown")]
    Unknown,
}

/// A repo's browsable pages, derived from its `origin` remote. `issues` / `pulls` are `None` on a
/// forge whose layout we don't know, so a caller renders what exists rather than a broken guess.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgeLinks {
    pub forge: Forge,
    /// The repo's code page — the one link every recognized remote yields.
    pub repo: String,
    /// Issues (GitHub / GitLab) or the project's work items (Azure DevOps).
    pub issues: Option<String>,
    /// Pull requests, or merge requests on GitLab.
    pub pulls: Option<String>,
}

/// The `origin` URL recorded in a clone's `config`, or `None` when the file is unreadable or has no
/// `origin`. `git_dir` is the git directory itself (`<repo>.git` for the bare clones bn manages).
/// A hand-rolled read of the one key we need — `[remote "origin"] url` — not a git-config parser:
/// section headers and `key = value` lines are all this has to understand.
pub fn origin_url(git_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(git_dir.join("config")).ok()?;
    let mut in_origin = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            // `[remote "origin"]`, tolerating the spacing git itself never writes but a human might.
            in_origin = section.replace(char::is_whitespace, "") == r#"remote"origin""#;
            continue;
        }
        if !in_origin {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == "url" {
            let url = value.trim();
            return (!url.is_empty()).then(|| url.to_string());
        }
    }
    None
}

/// The browsable pages of the clone at `git_dir` — [`origin_url`] resolved through [`links_from_url`].
pub fn links_for(git_dir: &Path) -> Option<ForgeLinks> {
    links_from_url(&origin_url(git_dir)?)
}

/// A remote URL split into its host and path, with the scheme, any `user@` and any trailing `.git`
/// removed. Handles both git remote shapes: scp-like (`git@host:path`) and URL-like
/// (`https://host/path`, `ssh://git@host:22/path`, `git://host/path`).
fn split_remote(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    let rest = match url.split_once("://") {
        Some((_scheme, rest)) => rest,
        // scp-like has no scheme: everything before the first `:` is `[user@]host`.
        None => {
            let (host, path) = url.split_once(':')?;
            let host = host.rsplit('@').next()?;
            return finish_split(host, path);
        }
    };
    let (authority, path) = rest.split_once('/')?;
    let host = authority.rsplit('@').next()?;
    // An `ssh://host:22/path` port is addressing, not identity — it never belongs in a web URL.
    let host = host.split(':').next()?;
    finish_split(host, path)
}

/// Normalize an already-separated host and path: lowercase the host, strip the path's leading and
/// trailing slashes and its `.git` suffix. `None` if either side came out empty.
fn finish_split(host: &str, path: &str) -> Option<(String, String)> {
    let host = host.trim().to_ascii_lowercase();
    let path = path.trim().trim_matches('/');
    let path = path
        .strip_suffix(".git")
        .unwrap_or(path)
        .trim_end_matches('/');
    (!host.is_empty() && !path.is_empty()).then(|| (host, path.to_string()))
}

/// Derive a repo's browsable pages from a remote URL, or `None` when the URL doesn't parse as a
/// remote at all. Pure — the whole point, so every forge shape is testable without a clone on disk.
pub fn links_from_url(url: &str) -> Option<ForgeLinks> {
    let (host, path) = split_remote(url)?;
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    if host == "github.com" {
        // `owner/repo`; deeper paths are a mistake, not a subgroup — GitHub has no nesting.
        let repo = format!("https://github.com/{}/{}", segs.first()?, segs.get(1)?);
        return Some(ForgeLinks {
            forge: Forge::GitHub,
            issues: Some(format!("{repo}/issues")),
            pulls: Some(format!("{repo}/pulls")),
            repo,
        });
    }

    if host == "dev.azure.com" || host == "ssh.dev.azure.com" || host.ends_with(".visualstudio.com")
    {
        return azure_links(&host, &segs);
    }

    // GitLab's own host and the self-hosted instances that keep the name; anything else self-hosted
    // is indistinguishable from a generic remote, and guessing `/-/issues` at it would be a 404.
    if host == "gitlab.com" || host.starts_with("gitlab.") {
        // Groups nest arbitrarily deep, so the whole path is the project.
        let repo = format!("https://{host}/{path}");
        return Some(ForgeLinks {
            forge: Forge::GitLab,
            issues: Some(format!("{repo}/-/issues")),
            pulls: Some(format!("{repo}/-/merge_requests")),
            repo,
        });
    }

    Some(ForgeLinks {
        forge: Forge::Unknown,
        repo: format!("https://{host}/{path}"),
        issues: None,
        pulls: None,
    })
}

/// The browser URL for one issue / work item id, or `None` when this forge's layout cannot name
/// one. Follows the module's rule: an absent link beats a link that 404s.
///
/// Only a bare number is an id — a leading `#` is trimmed, anything else is refused rather than
/// pasted into a path. On GitHub the issue path is also right for a pull request, since
/// `/issues/{n}` redirects to `/pull/{n}`; Azure DevOps is the unambiguous case, and the one a
/// work item actually comes from.
pub fn item_url(links: &ForgeLinks, id: &str) -> Option<String> {
    let n = id.trim().trim_start_matches('#');
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let issues = links.issues.as_deref()?;
    match links.forge {
        Forge::AzureDevOps => Some(format!("{issues}/edit/{n}")),
        Forge::GitHub | Forge::GitLab => Some(format!("{issues}/{n}")),
        Forge::Unknown => None,
    }
}

/// Azure DevOps links from an already-split remote. Three remote shapes point at the same pages:
/// `dev.azure.com/{org}/{project}/_git/{repo}`, the ssh form `ssh.dev.azure.com/v3/{org}/{project}/
/// {repo}`, and the legacy `{org}.visualstudio.com/{project}/_git/{repo}`. Work items are a project
/// concern rather than a repo one, so the issue link lands on the project's board — the closest
/// honest equivalent of a GitHub issue list.
fn azure_links(host: &str, segs: &[&str]) -> Option<ForgeLinks> {
    let (base, project) = if host == "ssh.dev.azure.com" {
        // `v3/{org}/{project}/{repo}` — no `_git` marker in the ssh form.
        let mut it = segs.iter().skip_while(|s| **s == "v3");
        let org = it.next()?;
        let project = it.next()?;
        let repo = it.next()?;
        (
            format!("https://dev.azure.com/{org}/{project}/_git/{repo}"),
            format!("https://dev.azure.com/{org}/{project}"),
        )
    } else {
        // `_git` separates the project path from the repo in both https forms; the segments before
        // it are the project path, which on the legacy host may carry a leading collection.
        let cut = segs.iter().position(|s| *s == "_git")?;
        let repo = segs.get(cut + 1)?;
        let project = segs[..cut].join("/");
        if project.is_empty() {
            return None;
        }
        (
            format!("https://{host}/{project}/_git/{repo}"),
            format!("https://{host}/{project}"),
        )
    };
    Some(ForgeLinks {
        forge: Forge::AzureDevOps,
        issues: Some(format!("{project}/_workitems")),
        pulls: Some(format!("{base}/pullrequests")),
        repo: base,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links(remote: &str) -> ForgeLinks {
        links_from_url(remote).expect("remote parses")
    }

    #[test]
    fn an_azure_work_item_id_becomes_its_edit_page() {
        let l = links("https://dev.azure.com/contoso/Project/_git/BigMono");
        assert_eq!(l.forge, Forge::AzureDevOps);
        assert_eq!(
            item_url(&l, "11047593").as_deref(),
            Some("https://dev.azure.com/contoso/Project/_workitems/edit/11047593")
        );
    }

    #[test]
    fn the_ssh_and_legacy_azure_remotes_reach_the_same_work_item() {
        let want = Some("https://dev.azure.com/contoso/Project/_workitems/edit/42".to_string());
        assert_eq!(
            item_url(
                &links("git@ssh.dev.azure.com:v3/contoso/Project/BigMono"),
                "42"
            ),
            want
        );
        let legacy = links("https://contoso.visualstudio.com/Project/_git/BigMono");
        assert_eq!(
            item_url(&legacy, "42").as_deref(),
            Some("https://contoso.visualstudio.com/Project/_workitems/edit/42")
        );
    }

    #[test]
    fn github_and_gitlab_use_their_issue_path() {
        assert_eq!(
            item_url(&links("git@github.com:eduuh/bn.git"), "1250").as_deref(),
            Some("https://github.com/eduuh/bn/issues/1250")
        );
        assert_eq!(
            item_url(&links("https://gitlab.com/group/sub/proj"), "7").as_deref(),
            Some("https://gitlab.com/group/sub/proj/-/issues/7")
        );
    }

    #[test]
    fn a_hash_prefix_and_a_bare_number_agree() {
        let l = links("git@github.com:eduuh/bn.git");
        assert_eq!(item_url(&l, "#338"), item_url(&l, "338"));
    }

    #[test]
    fn an_unknown_forge_names_no_item_rather_than_guessing() {
        let l = links("git@git.example.com:team/thing.git");
        assert_eq!(l.forge, Forge::Unknown);
        assert!(item_url(&l, "1").is_none());
    }

    #[test]
    fn anything_that_is_not_a_number_is_refused() {
        let l = links("git@github.com:eduuh/bn.git");
        for bad in ["", "  ", "#", "abc", "12a", "1/2", "../etc"] {
            assert!(item_url(&l, bad).is_none(), "accepted {bad:?}");
        }
    }
}
