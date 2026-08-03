//! Where a repo lives in a browser: its `origin` remote resolved to the forge's code, issue and
//! pull-request pages, so the dashboard can hand you the link instead of making you reconstruct it.
//!
//! The remote is read straight out of the clone's `config` file rather than by shelling out to
//! `git remote get-url`. `/api/repos` derives links for every repo on the machine on a 30-second
//! poll; a process spawn per repo per poll is not a price a read this routine should charge.
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

    fn links(url: &str) -> ForgeLinks {
        links_from_url(url).unwrap_or_else(|| panic!("no links for {url}"))
    }

    #[test]
    fn github_ssh_and_https_remotes_agree() {
        for url in [
            "git@github.com:eduuh/bn.git",
            "https://github.com/eduuh/bn.git",
            "https://github.com/eduuh/bn",
            "ssh://git@github.com:22/eduuh/bn.git",
            "https://eduuh@github.com/eduuh/bn.git",
        ] {
            let l = links(url);
            assert_eq!(l.forge, Forge::GitHub, "{url}");
            assert_eq!(l.repo, "https://github.com/eduuh/bn", "{url}");
            assert_eq!(
                l.issues.as_deref(),
                Some("https://github.com/eduuh/bn/issues")
            );
            assert_eq!(
                l.pulls.as_deref(),
                Some("https://github.com/eduuh/bn/pulls")
            );
        }
    }

    #[test]
    fn azure_devops_https_ssh_and_legacy_forms_agree() {
        // The modern https form, the org-as-userinfo variant, and the ssh form all name one repo.
        for url in [
            "https://dev.azure.com/contoso/Payments/_git/api",
            "https://contoso@dev.azure.com/contoso/Payments/_git/api",
            "git@ssh.dev.azure.com:v3/contoso/Payments/api",
        ] {
            let l = links(url);
            assert_eq!(l.forge, Forge::AzureDevOps, "{url}");
            assert_eq!(
                l.repo, "https://dev.azure.com/contoso/Payments/_git/api",
                "{url}"
            );
            assert_eq!(
                l.pulls.as_deref(),
                Some("https://dev.azure.com/contoso/Payments/_git/api/pullrequests"),
                "{url}"
            );
            // Work items are the project's, not the repo's.
            assert_eq!(
                l.issues.as_deref(),
                Some("https://dev.azure.com/contoso/Payments/_workitems"),
                "{url}"
            );
        }
    }

    #[test]
    fn azure_legacy_host_keeps_its_own_domain() {
        // Rewriting a working visualstudio.com remote to dev.azure.com would be a guess about the
        // org's migration state; keep the host the remote actually names.
        let l = links("https://contoso.visualstudio.com/DefaultCollection/Payments/_git/api");
        assert_eq!(l.forge, Forge::AzureDevOps);
        assert_eq!(
            l.repo,
            "https://contoso.visualstudio.com/DefaultCollection/Payments/_git/api"
        );
        assert_eq!(
            l.issues.as_deref(),
            Some("https://contoso.visualstudio.com/DefaultCollection/Payments/_workitems")
        );
    }

    #[test]
    fn gitlab_uses_its_dash_paths_and_keeps_nested_groups() {
        let l = links("git@gitlab.com:group/sub/proj.git");
        assert_eq!(l.forge, Forge::GitLab);
        assert_eq!(l.repo, "https://gitlab.com/group/sub/proj");
        assert_eq!(
            l.issues.as_deref(),
            Some("https://gitlab.com/group/sub/proj/-/issues")
        );
        assert_eq!(
            l.pulls.as_deref(),
            Some("https://gitlab.com/group/sub/proj/-/merge_requests")
        );
    }

    #[test]
    fn unknown_host_gets_a_code_link_and_no_invented_pages() {
        let l = links("git@git.example.com:team/thing.git");
        assert_eq!(l.forge, Forge::Unknown);
        assert_eq!(l.repo, "https://git.example.com/team/thing");
        assert_eq!(l.issues, None, "never guess an unknown forge's issue path");
        assert_eq!(l.pulls, None);
    }

    #[test]
    fn unparseable_remotes_yield_nothing() {
        for url in [
            "",
            "   ",
            "not-a-remote",
            "https://github.com/",
            "git@github.com:",
        ] {
            assert!(links_from_url(url).is_none(), "{url:?} should not parse");
        }
        // A host with no repo path is not a repo link.
        assert!(links_from_url("https://dev.azure.com/contoso").is_none());
    }

    #[test]
    fn origin_url_reads_the_bare_clones_config() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join("bn.git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(
            git_dir.join("config"),
            "[core]\n\tbare = true\n\turl = decoy\n[remote \"upstream\"]\n\turl = git@github.com:other/bn.git\n\
             [remote \"origin\"]\n\turl = git@github.com:eduuh/bn.git\n\tfetch = +refs/heads/*\n",
        )
        .unwrap();
        // The `url` keys under `[core]` and `[remote "upstream"]` must not win.
        assert_eq!(
            origin_url(&git_dir).as_deref(),
            Some("git@github.com:eduuh/bn.git")
        );
        assert_eq!(
            links_for(&git_dir).unwrap().repo,
            "https://github.com/eduuh/bn"
        );
    }

    #[test]
    fn a_clone_with_no_origin_has_no_links() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join("local.git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("config"), "[core]\n\tbare = true\n").unwrap();
        assert_eq!(origin_url(&git_dir), None);
        assert_eq!(links_for(&git_dir), None);
        // A git dir that isn't there at all is the same answer, not a panic.
        assert_eq!(origin_url(&dir.path().join("absent.git")), None);
    }
}
