//! `bn links check`: is each stored link still worth opening? The decisions live here, pure and
//! unit-tested; the CLI runs `gh api` / `curl` and hands the raw answers back to these.
//!
//! The one rule that shapes everything: a page that wants you signed in is not dead. Azure
//! DevOps and the portal both answer an anonymous request with 401, 403 or a redirect to a
//! login host, and calling those dead would make the report useless for exactly the links a
//! work store is full of.

use super::host;

/// What a check found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// Reachable, or an open PR or issue.
    Ok,
    /// A merged pull request.
    Merged,
    /// A closed (unmerged) pull request, or a closed issue.
    Closed,
    /// 404 or 410: gone.
    Dead,
    /// Reachable only when signed in — not a verdict on the link.
    SignIn,
    /// The check itself failed (timeout, DNS, no `gh`/`curl`).
    Unknown,
}

impl Health {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Merged => "merged",
            Self::Closed => "closed",
            Self::Dead => "dead",
            Self::SignIn => "sign-in",
            Self::Unknown => "unknown",
        }
    }
}

/// How to check one URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// A GitHub PR or issue: ask the API for its state (`gh api <path>`).
    Github(String),
    /// Anything else: an HTTP request.
    Http,
}

/// A github.com PR or issue URL becomes its API path; everything else is an HTTP probe.
pub fn probe_for(url: &str) -> Probe {
    if host(url) != "github.com" {
        return Probe::Http;
    }
    let path = url
        .split_once("github.com/")
        .map(|(_, p)| p)
        .unwrap_or("")
        .split(['?', '#'])
        .next()
        .unwrap_or("");
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        [owner, repo, kind @ ("pull" | "issues"), n, ..]
            if n.chars().all(|c| c.is_ascii_digit()) =>
        {
            let api = if *kind == "pull" { "pulls" } else { "issues" };
            Probe::Github(format!("repos/{owner}/{repo}/{api}/{n}"))
        }
        [owner, repo, "releases", "tag", tag, ..] if !tag.is_empty() => {
            Probe::Github(format!("repos/{owner}/{repo}/releases/tags/{tag}"))
        }
        _ => Probe::Http,
    }
}

/// A `gh api` answer: `state` (`open`/`closed`, or `exists` for a release, which has none) and
/// `merged` from the JSON, or `not_found` when gh reported a 404. Anything else (no gh, no auth,
/// rate limit) is unknown.
pub fn classify_github(state: Option<&str>, merged: bool, not_found: bool) -> Health {
    if not_found {
        return Health::Dead;
    }
    match state {
        Some("open" | "exists") => Health::Ok,
        Some("closed") if merged => Health::Merged,
        Some("closed") => Health::Closed,
        _ => Health::Unknown,
    }
}

/// Hosts a signed-out request is redirected to. Landing on one means "sign in", not "gone".
const LOGIN_HOSTS: &[&str] = &[
    "login.microsoftonline.com",
    "login.live.com",
    "login.windows.net",
    "aadcdn.msftauth.net",
];

/// A `curl -L` answer: the final status code and the URL it ended on. `None` status means the
/// request never completed.
///
/// A 404 from github.com is unknown, not dead: GitHub answers a signed-out request for anything
/// in a private repo with 404, so it cannot tell "gone" from "private". PRs, issues and releases
/// never reach here — [`probe_for`] sends them through the signed-in API.
pub fn classify_http(status: Option<u16>, final_url: Option<&str>) -> Health {
    let landed_on_login = final_url.is_some_and(|u| {
        let h = host(u);
        LOGIN_HOSTS.contains(&h.as_str()) || (h == "github.com" && u.contains("github.com/login"))
    });
    let on_github = final_url.is_some_and(|u| host(u) == "github.com");
    match status {
        _ if landed_on_login => Health::SignIn,
        Some(404) if on_github => Health::Unknown,
        Some(401 | 403) => Health::SignIn,
        Some(404 | 410) => Health::Dead,
        Some(200..=399) => Health::Ok,
        _ => Health::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_prs_and_issues_go_to_the_api_and_everything_else_to_http() {
        assert_eq!(
            probe_for("https://github.com/eduuh/bn/pull/1470"),
            Probe::Github("repos/eduuh/bn/pulls/1470".into())
        );
        assert_eq!(
            probe_for("https://github.com/eduuh/bn/issues/9#issuecomment-1"),
            Probe::Github("repos/eduuh/bn/issues/9".into())
        );
        assert_eq!(
            probe_for("https://github.com/eduuh/wira360/releases/tag/forge-v0.6.0"),
            Probe::Github("repos/eduuh/wira360/releases/tags/forge-v0.6.0".into())
        );
        assert_eq!(probe_for("https://github.com/eduuh/bn"), Probe::Http);
        assert_eq!(
            probe_for("https://dev.azure.com/o/p/_workitems/edit/1"),
            Probe::Http
        );
    }

    #[test]
    fn a_pr_reads_as_open_merged_closed_or_dead() {
        assert_eq!(classify_github(Some("open"), false, false), Health::Ok);
        assert_eq!(classify_github(Some("closed"), true, false), Health::Merged);
        assert_eq!(
            classify_github(Some("closed"), false, false),
            Health::Closed
        );
        assert_eq!(classify_github(None, false, true), Health::Dead);
        assert_eq!(classify_github(None, false, false), Health::Unknown);
    }

    #[test]
    fn a_page_behind_sign_in_is_never_called_dead() {
        assert_eq!(classify_http(Some(401), None), Health::SignIn);
        assert_eq!(classify_http(Some(403), None), Health::SignIn);
        assert_eq!(
            classify_http(
                Some(200),
                Some("https://login.microsoftonline.com/common/oauth2")
            ),
            Health::SignIn
        );
        assert_eq!(classify_http(Some(404), None), Health::Dead);
        assert_eq!(
            classify_http(Some(404), Some("https://github.com/eduuh/private-repo")),
            Health::Unknown,
            "GitHub hides private pages behind 404"
        );
        assert_eq!(classify_github(Some("exists"), false, false), Health::Ok);
        assert_eq!(classify_http(Some(410), None), Health::Dead);
        assert_eq!(
            classify_http(Some(200), Some("https://example.com/")),
            Health::Ok
        );
        assert_eq!(classify_http(None, None), Health::Unknown);
        assert_eq!(classify_http(Some(500), None), Health::Unknown);
    }
}
