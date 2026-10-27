//! Remote-host awareness for `wt`: origin URL parsing (GitHub vs ADO), PR state
//! queries, the my-open-PRs listings, and ADO branch favouriting. Moved verbatim
//! from the former wt.rs god file (issue #351).

use super::*;
use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Azure DevOps' AAD resource id — passed to `az rest --resource` to acquire a token for
/// `dev.azure.com` REST calls (works with an `az login` AAD session).
pub(crate) const ADO_AAD_RESOURCE: &str = "499b84ac-1321-427f-aa17-267ca6975798";

static ADO_CREATOR_CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

/// The hosting service a bare repo's origin points at — what `cleanup` queries for PR state and
/// `merge` drives. ADO (`completed` == merged) is detected alongside GitHub so the poller catches
/// work-repo merges too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Remote {
    GitHub {
        slug: String,
    },
    Ado {
        org: String,
        project: String,
        repo: String,
    },
    Other,
}

impl Remote {
    /// Human label for status lines: `owner/repo` (GitHub) or `org/project/repo` (ADO).
    pub(crate) fn label(&self) -> String {
        match self {
            Remote::GitHub { slug } => slug.clone(),
            Remote::Ado { org, project, repo } => format!("{org}/{project}/{repo}"),
            Remote::Other => "?".to_string(),
        }
    }
}

/// Classify a bare repo by its origin URL.
pub(crate) fn remote_for(bare: &Path) -> Remote {
    git_out(&[
        "--git-dir",
        &bare.to_string_lossy(),
        "remote",
        "get-url",
        "origin",
    ])
    .map(|u| parse_remote(&u))
    .unwrap_or(Remote::Other)
}

/// Parse a git remote URL into a [`Remote`]. Pure (testable). Handles GitHub
/// (`git@github.com:o/r.git`, `https://github.com/o/r`), Azure DevOps
/// (`https://dev.azure.com/org/proj/_git/repo`, `git@ssh.dev.azure.com:v3/org/proj/repo`), and the
/// legacy `https://org.visualstudio.com/proj/_git/repo` form.
pub(crate) fn parse_remote(url: &str) -> Remote {
    if url.contains("github.com") {
        return match slug_from_url(url) {
            Some(slug) => Remote::GitHub { slug },
            None => Remote::Other,
        };
    }
    if url.contains("dev.azure.com") {
        // After the host: "/org/proj/_git/repo(.git)" or ":v3/org/proj/repo".
        if let Some(rest) = url.split("dev.azure.com").nth(1) {
            let rest = rest.trim_start_matches([':', '/']);
            let rest = rest.strip_prefix("v3/").unwrap_or(rest);
            let parts: Vec<&str> = rest
                .trim_end_matches(".git")
                .split('/')
                .filter(|s| {
                    !s.is_empty() && *s != "_git" && !s.eq_ignore_ascii_case("DefaultCollection")
                })
                .collect();
            if parts.len() >= 3 {
                return Remote::Ado {
                    org: parts[0].to_string(),
                    project: parts[1].to_string(),
                    repo: parts[2].to_string(),
                };
            }
        }
        return Remote::Other;
    }
    if url.contains("visualstudio.com") {
        // host = "<org>.visualstudio.com"; path = "/proj/_git/repo".
        let org = url
            .split("visualstudio.com")
            .next()
            .and_then(|b| b.trim_end_matches('.').rsplit(['/', '@', '.']).next())
            .unwrap_or("")
            .to_string();
        let parts: Vec<&str> = url
            .split("visualstudio.com")
            .nth(1)
            .unwrap_or("")
            .trim_end_matches(".git")
            .split('/')
            .filter(|s| {
                !s.is_empty() && *s != "_git" && !s.eq_ignore_ascii_case("DefaultCollection")
            })
            .collect();
        if !org.is_empty() && parts.len() >= 2 {
            return Remote::Ado {
                org,
                project: parts[0].to_string(),
                repo: parts[1].to_string(),
            };
        }
    }
    Remote::Other
}

/// owner/repo from a GitHub remote URL. Handles `git@github.com:owner/repo.git` and the
/// `https://github.com/owner/repo(.git)` form.
pub(crate) fn slug_from_url(url: &str) -> Option<String> {
    let after = url.split("github.com").nth(1)?;
    let after = after.trim_start_matches([':', '/']);
    let slug = after.trim_end_matches(".git");
    let mut parts = slug.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if owner.is_empty() || repo.is_empty() {
        None
    } else {
        Some(format!("{owner}/{repo}"))
    }
}

/// PR state for `<branch>` on `remote`: one of MERGED / OPEN / CLOSED / NONE.
pub(crate) fn pr_state(remote: &Remote, branch: &str) -> String {
    match remote {
        Remote::GitHub { slug } => gh_pr_state(slug, branch),
        Remote::Ado { org, project, repo } => {
            ado_pr_state(AdoTarget { org, project, repo }, branch)
        }
        Remote::Other => "NONE".to_string(),
    }
}

pub(crate) fn gh_pr_state(slug: &str, branch: &str) -> String {
    #[derive(serde::Deserialize)]
    struct StateRow {
        state: String,
    }
    bn_core::runner::gh_json::<Vec<StateRow>>(
        &bn_core::runner::RealRunner,
        &[
            "pr", "list", "--repo", slug, "--head", branch, "--state", "all", "--json", "state",
        ],
    )
    .unwrap_or_default()
    .first()
    .map(|r| r.state.clone())
    .filter(|s| !s.is_empty())
    .unwrap_or_else(|| "NONE".to_string())
}

/// The Azure DevOps coordinates that address a repo — org / project / repo — as one aggregate
/// (mirroring the [`Remote::Ado`] variant). Passing this instead of three adjacent
/// `org`/`project`/`repo` `&str` params stops a silent argument swap at the call boundary.
#[derive(Clone, Copy)]
pub(crate) struct AdoTarget<'a> {
    pub(crate) org: &'a str,
    pub(crate) project: &'a str,
    pub(crate) repo: &'a str,
}

/// ADO PR status via `az repos pr list`. ADO's lifecycle states map onto ours: `completed` is a
/// merge, `active` is open, `abandoned` is a close-without-merge.
pub(crate) fn ado_pr_state(t: AdoTarget<'_>, branch: &str) -> String {
    let AdoTarget { org, project, repo } = t;
    let out = Command::new("az")
        .args([
            "repos",
            "pr",
            "list",
            "--organization",
            &format!("https://dev.azure.com/{org}"),
            "--project",
            project,
            "--repository",
            repo,
            "--source-branch",
            branch,
            "--status",
            "all",
            "--query",
            "[0].status",
            "--output",
            "tsv",
        ])
        .output();
    normalize_state(out, |s| match s.to_ascii_lowercase().as_str() {
        "completed" => "MERGED".to_string(),
        "active" => "OPEN".to_string(),
        "abandoned" => "CLOSED".to_string(),
        _ => "NONE".to_string(),
    })
}

/// Shared decoder: trimmed stdout → mapped state, with empty / failure / "None" → NONE.
pub(crate) fn normalize_state(
    out: std::io::Result<std::process::Output>,
    map: impl Fn(&str) -> String,
) -> String {
    match out {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() || s == "None" {
                "NONE".to_string()
            } else {
                map(&s)
            }
        }
        _ => "NONE".to_string(),
    }
}

/// Head commit SHAs of the *merged* PRs whose source branch is `branch`. `pr_state` matches a PR by
/// branch NAME only, so a name reused after an old merged PR — or a branch carrying commits past the
/// merge — reads as MERGED even though the current tip never shipped. `cleanup_one` compares the
/// worktree's `HEAD` against this set before force-deleting the branch, sparing that unpushed work.
/// Returns empty on any error or an unhosted remote, so the caller fails closed (requires --force).
pub(crate) fn merged_pr_heads(remote: &Remote, branch: &str) -> Vec<String> {
    let out = match remote {
        Remote::GitHub { slug } => crate::gh::gh_command(&[
            "pr",
            "list",
            "--repo",
            slug,
            "--head",
            branch,
            "--state",
            "merged",
            "--json",
            "headRefOid",
            "--jq",
            ".[].headRefOid",
        ])
        .output(),
        Remote::Ado { org, project, repo } => Command::new("az")
            .args([
                "repos",
                "pr",
                "list",
                "--organization",
                &format!("https://dev.azure.com/{org}"),
                "--project",
                project,
                "--repository",
                repo,
                "--source-branch",
                branch,
                "--status",
                "completed",
                "--query",
                "[].lastMergeSourceCommit.commitId",
                "--output",
                "tsv",
            ])
            .output(),
        Remote::Other => return Vec::new(),
    };
    match out {
        Ok(o) if o.status.success() => oid_lines(&String::from_utf8_lossy(&o.stdout)),
        _ => Vec::new(),
    }
}

// --- remote PRs (my open PRs across machines) ------------------------------

/// One of my open PRs on the hosting service — the source for `wt remote`. The `branch` is the PR's
/// head/source branch, which is what becomes a local worktree.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(crate) struct RemotePr {
    pub(crate) number: i64,
    pub(crate) title: String,
    pub(crate) branch: String,
    pub(crate) url: String,
}

impl RemotePr {
    /// Whether `target` (a PR number, `#number`, or the source branch) names this PR.
    pub(crate) fn matches(&self, target: &str) -> bool {
        let t = target.trim();
        t.trim_start_matches('#') == self.number.to_string() || t == self.branch
    }

    /// fzf row: `#<number>  <title>  [<branch>]`. The leading `#<number>` token is parsed back by
    /// [`number_from_fzf_line`] to recover the selection.
    pub(crate) fn fzf_line(&self) -> String {
        format!("#{}  {}  [{}]", self.number, self.title, self.branch)
    }
}

/// Recover a PR number from a [`RemotePr::fzf_line`] selection (`#<number>  …`).
pub(crate) fn number_from_fzf_line(line: &str) -> Option<i64> {
    line.trim()
        .trim_start_matches('#')
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// My open PRs on `remote`, via the host CLI. Empty on any error / unsupported host so the command
/// degrades to "nothing to fetch" rather than failing.
pub(crate) fn my_open_prs(remote: &Remote) -> Vec<RemotePr> {
    match remote {
        Remote::GitHub { slug } => gh_my_open_prs(slug),
        Remote::Ado { org, project, repo } => ado_my_open_prs(AdoTarget { org, project, repo }),
        Remote::Other => Vec::new(),
    }
}

/// My open GitHub PRs for `slug` via `gh pr list --author @me`. `@me` is the gh-authenticated user,
/// so this naturally scopes to PRs I opened — including ones pushed from another machine.
pub(crate) fn gh_my_open_prs(slug: &str) -> Vec<RemotePr> {
    let out = bn_core::runner::RealRunner.gh(&[
        "pr",
        "list",
        "--repo",
        slug,
        "--author",
        "@me",
        "--state",
        "open",
        "--limit",
        "100",
        "--json",
        "number,title,headRefName,url",
    ]);
    if !out.success {
        let detail = out.stderr.trim();
        if detail.is_empty() {
            ceprintln!("{YELLOW}remote PR: GitHub query failed — run `gh auth status`{NC}");
        } else {
            ceprintln!(
                "{YELLOW}remote PR: GitHub query failed: {detail} — run `gh auth status`{NC}"
            );
        }
        return Vec::new();
    }
    parse_gh_prs(&out.stdout)
}

/// Parse `gh pr list --json number,title,headRefName,url` output into [`RemotePr`]s. Pure (testable).
pub(crate) fn parse_gh_prs(json: &str) -> Vec<RemotePr> {
    let items: Vec<serde_json::Value> = serde_json::from_str(json).unwrap_or_default();
    items
        .iter()
        .filter_map(|p| {
            Some(RemotePr {
                number: p.get("number")?.as_i64()?,
                title: p
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                branch: p.get("headRefName")?.as_str()?.to_string(),
                url: p
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect()
}

/// My active ADO PRs for `org/project/repo`, via the REST `pullrequests` API filtered by
/// `searchCriteria.creatorId` (my identity from [`ado_creator`]). We use direct REST with a GCM
/// token from `git credential fill` rather than `az repos pr list --creator`, whose identity-picker
/// resolution is unreliable across projects and whose Windows startup cost is slow. Empty — with a
/// warning — on any failure, so the command degrades to "nothing to fetch" rather than aborting.
pub(crate) fn ado_my_open_prs(t: AdoTarget<'_>) -> Vec<RemotePr> {
    let AdoTarget { org, project, .. } = t;
    let org_url = format!("https://dev.azure.com/{org}");
    let repo_path = ado_repo_path(t);
    let Some(creator) = ado_creator(&org_url, &repo_path) else {
        ceprintln!(
            "{YELLOW}remote PR: couldn't resolve your Azure DevOps identity — set \
             BN_ADO_CREATOR=<identity-guid> or ensure `git credential fill` can reach dev.azure.com{NC}"
        );
        return Vec::new();
    };
    // Project-scoped query (returns every repo's PRs); we filter to `repo` in parse_ado_prs.
    let url = format!(
        "{org_url}/{project}/_apis/git/pullrequests\
         ?searchCriteria.creatorId={creator}&searchCriteria.status=active&api-version=7.1"
    );
    if let Some(body) = ado_rest_json(&repo_path, &url) {
        return parse_ado_prs(&body, t);
    }
    let out = Command::new("az")
        .args([
            "rest",
            "--resource",
            ADO_AAD_RESOURCE,
            "--url",
            &url,
            "--output",
            "json",
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => parse_ado_prs(&String::from_utf8_lossy(&o.stdout), t),
        Ok(o) => {
            ceprintln!(
                "{YELLOW}remote PR: Azure DevOps query failed: {}{NC}",
                String::from_utf8_lossy(&o.stderr).trim()
            );
            Vec::new()
        }
        Err(e) => {
            ceprintln!("{YELLOW}remote PR: couldn't run `az rest`: {e}{NC}");
            Vec::new()
        }
    }
}

/// Parse an ADO `pullrequests` REST response (`{"value": [...]}`, or a bare array) into
/// [`RemotePr`]s, keeping only PRs whose `repository.name` matches `repo` (the project-scoped query
/// returns every repo's PRs; items without a `repository.name` are kept for test fixtures).
/// Synthesizes each PR's web URL from `org/project/repo` + id. Pure (testable).
/// `sourceRefName` is `refs/heads/<branch>`.
pub(crate) fn parse_ado_prs(json: &str, t: AdoTarget<'_>) -> Vec<RemotePr> {
    let AdoTarget { org, project, repo } = t;
    let root: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let items: Vec<serde_json::Value> = match root {
        serde_json::Value::Array(a) => a,
        serde_json::Value::Object(o) => o
            .get("value")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    items
        .iter()
        .filter(|p| {
            p.get("repository")
                .and_then(|r| r.get("name"))
                .and_then(|n| n.as_str())
                .is_none_or(|name| name.eq_ignore_ascii_case(repo))
        })
        .filter_map(|p| {
            let number = p.get("pullRequestId")?.as_i64()?;
            let branch = p
                .get("sourceRefName")?
                .as_str()?
                .trim_start_matches("refs/heads/")
                .to_string();
            Some(RemotePr {
                number,
                title: p
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                branch,
                url: format!(
                    "https://dev.azure.com/{org}/{project}/_git/{repo}/pullrequest/{number}"
                ),
            })
        })
        .collect()
}

/// Make an open PR's source branch available in the bare repository without deciding where it
/// should be checked out. ADO limited-ref repositories need the branch favourited before fetch;
/// GitHub and ordinary ADO repositories go straight to the exact branch refspec.
pub(crate) fn prepare_remote_pr_branch(remote: &Remote, bare_s: &str, pr: &RemotePr) -> bool {
    if matches!(remote, Remote::Ado { .. }) && !favourite_branch(remote, &pr.branch) {
        ceprintln!(
            "{YELLOW}remote PR: could not favourite {} — fetch may fail on a limited-ref repo{NC}",
            pr.branch
        );
    }

    cprintln!("{BLUE}Fetching PR #{} ({})...{NC}", pr.number, pr.branch);
    if fetch_branch(bare_s, &pr.branch) {
        true
    } else {
        ceprintln!(
            "{RED}remote PR: failed to fetch {} for PR #{}{NC}",
            pr.branch, pr.number
        );
        false
    }
}

/// My Azure DevOps identity GUID, used to filter PRs (`searchCriteria.creatorId`): `BN_ADO_CREATOR`
/// if set, else `authenticatedUser.id` from the org's `connectionData` using the same GCM token
/// as the other ADO REST calls. Cached per org for the life of the process. `None` when neither
/// is available.
pub(crate) fn ado_creator(org_url: &str, repo_path: &str) -> Option<String> {
    if let Ok(c) = std::env::var("BN_ADO_CREATOR") {
        let c = c.trim().to_string();
        if !c.is_empty() {
            return Some(c);
        }
    }
    if let Ok(cache) = ADO_CREATOR_CACHE.get_or_init(|| Mutex::new(HashMap::new())).lock() {
        if let Some(id) = cache.get(org_url).cloned() {
            return Some(id);
        }
    }
    if let Some(cache) = read_ado_creator_cache()
        .and_then(|cache| cache.get(org_url).cloned())
    {
        if let Ok(mut mem) = ADO_CREATOR_CACHE.get_or_init(|| Mutex::new(HashMap::new())).lock() {
            mem.insert(org_url.to_string(), cache.clone());
        }
        return Some(cache);
    }
    let url = format!("{org_url}/_apis/connectionData?api-version=7.1-preview");
    let body = ado_rest_json(repo_path, &url)?;
    let id = parse_ado_creator(&body)?;
    if let Ok(mut mem) = ADO_CREATOR_CACHE.get_or_init(|| Mutex::new(HashMap::new())).lock() {
        mem.insert(org_url.to_string(), id.clone());
    }
    upsert_ado_creator_cache(org_url, &id);
    Some(id)
}

fn ado_repo_path(t: AdoTarget<'_>) -> String {
    let AdoTarget { org, project, repo } = t;
    format!("{org}/{project}/_git/{repo}")
}

fn ado_gcm_token(repo_path: &str) -> Option<String> {
    static TOKEN_CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    if let Ok(cache) = TOKEN_CACHE.get_or_init(|| Mutex::new(HashMap::new())).lock() {
        if let Some(token) = cache.get(repo_path).cloned() {
            return Some(token);
        }
    }
    let mut child = Command::new("git")
        .args(["credential", "fill"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let input = format!("protocol=https\nhost=dev.azure.com\npath={repo_path}\n\n");
    child.stdin.as_mut()?.write_all(input.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let token = parse_git_credential_fill(&String::from_utf8_lossy(&out.stdout))?;
    if let Ok(mut cache) = TOKEN_CACHE.get_or_init(|| Mutex::new(HashMap::new())).lock() {
        cache.insert(repo_path.to_string(), token.clone());
    }
    Some(token)
}

fn parse_git_credential_fill(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix("password="))
        .map(str::trim)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn parse_ado_creator(json: &str) -> Option<String> {
    let root: serde_json::Value = serde_json::from_str(json).ok()?;
    root.get("authenticatedUser")
        .and_then(|u| u.get("id"))
        .and_then(|id| id.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn read_ado_creator_cache() -> Option<HashMap<String, String>> {
    let path = ado_creator_cache_path();
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn upsert_ado_creator_cache(org_url: &str, id: &str) {
    let path = ado_creator_cache_path();
    let mut cache = read_ado_creator_cache().unwrap_or_default();
    if cache.get(org_url).map(|s| s.as_str()) == Some(id) {
        return;
    }
    cache.insert(org_url.to_string(), id.to_string());
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(body) = serde_json::to_vec(&cache) {
        if std::fs::write(&path, body).is_ok() {
            set_private_permissions(&path);
        }
    }
}

fn ado_creator_cache_path() -> std::path::PathBuf {
    bn_core::resolver::state_dir().join("ado-creator-cache.json")
}

#[cfg(unix)]
fn set_private_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(path, perms);
    }
}

#[cfg(not(unix))]
fn set_private_permissions(_: &std::path::Path) {}

fn ado_rest_json(repo_path: &str, url: &str) -> Option<String> {
    let token = ado_gcm_token(repo_path)?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .build()
        .into();
    let resp = agent
        .get(url)
        .header("Authorization", &format!("Bearer {token}"))
        .call()
        .ok()?;
    if resp.status().as_u16() >= 400 {
        return None;
    }
    resp.into_body().read_to_string().ok()
}

/// Favourite an ADO branch via the Git Ref Favorites API (`_apis/git/repositories/{id}/
/// refsFavorites`). This — NOT the generic `favorite/favorites` service (which has no ref type) —
/// is what backs the ☆ on the Branches page, and on a limited-ref repo it's what makes the branch
/// fetchable. Idempotent: a no-op if already favourited. Returns whether the branch ends up
/// favourited. Caller guarantees `remote` is ADO.
pub(crate) fn favourite_branch(remote: &Remote, branch: &str) -> bool {
    let Remote::Ado { org, project, repo } = remote else {
        return false;
    };
    let org_url = format!("https://dev.azure.com/{org}");
    let route = format!("project={project}");

    let Some(repo_id) = az_tsv(&[
        "repos",
        "show",
        "--organization",
        &org_url,
        "--project",
        project,
        "--repository",
        repo,
        "--query",
        "id",
        "--output",
        "tsv",
    ]) else {
        ceprintln!("{YELLOW}favourite: could not resolve repo id for {project}/{repo}{NC}");
        return false;
    };

    if ref_favourite_exists(&org_url, &route, &repo_id, branch) {
        return true; // already favourited
    }

    let body = serde_json::json!({
        "name": format!("refs/heads/{branch}"),
        "repositoryId": repo_id,
        "type": "Ref",
    });
    let Some(tmp) = write_temp_json(&body) else {
        return false;
    };
    let ok = Command::new("az")
        .args([
            "devops",
            "invoke",
            "--organization",
            &org_url,
            "--area",
            "git",
            "--resource",
            "refsFavorites",
            "--route-parameters",
            &route,
            "--http-method",
            "POST",
            "--api-version",
            "7.1-preview",
            "--in-file",
            &tmp.to_string_lossy(),
            "--output",
            "none",
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let _ = std::fs::remove_file(&tmp);
    ok
}

/// Whether `branch` is already in the repo's ref favourites (so we don't POST a duplicate).
pub(crate) fn ref_favourite_exists(
    org_url: &str,
    route: &str,
    repo_id: &str,
    branch: &str,
) -> bool {
    let out = Command::new("az")
        .args([
            "devops",
            "invoke",
            "--organization",
            org_url,
            "--area",
            "git",
            "--resource",
            "refsFavorites",
            "--route-parameters",
            route,
            "--query-parameters",
            &format!("repositoryId={repo_id}"),
            "--http-method",
            "GET",
            "--api-version",
            "7.1-preview",
            "--output",
            "json",
        ])
        .output();
    let Ok(out) = out else { return false };
    if !out.status.success() {
        return false;
    }
    let want = format!("refs/heads/{branch}");
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .ok()
        .and_then(|v| v.get("value").and_then(|x| x.as_array().cloned()))
        .map(|arr| {
            arr.iter()
                .any(|f| f.get("name").and_then(|n| n.as_str()) == Some(want.as_str()))
        })
        .unwrap_or(false)
}

/// `az ... --output tsv` → trimmed stdout, or `None` on failure/empty.
pub(crate) fn az_tsv(args: &[&str]) -> Option<String> {
    let out = Command::new("az").args(args).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !s.is_empty()).then_some(s)
}

/// Write `value` to a temp JSON file under the state dir; returns its path.
pub(crate) fn write_temp_json(value: &serde_json::Value) -> Option<PathBuf> {
    let dir = bn_core::resolver::state_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("favourite-body.json");
    std::fs::write(&path, serde_json::to_vec(value).ok()?).ok()?;
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_fill_parser_reads_the_password_field() {
        let text = "\
protocol=https\n\
host=dev.azure.com\n\
username=git\n\
password=secret-token\n";
        assert_eq!(parse_git_credential_fill(text), Some("secret-token".to_string()));
    }

    #[test]
    fn ado_creator_parser_reads_the_authenticated_user_guid() {
        let json = r#"{"authenticatedUser":{"id":"abc-123"}}"#;
        assert_eq!(parse_ado_creator(json), Some("abc-123".to_string()));
    }

    #[test]
    fn ado_repo_path_matches_the_git_credential_input_shape() {
        let path = ado_repo_path(AdoTarget {
            org: "contoso",
            project: "Project",
            repo: "BigMono",
        });
        assert_eq!(path, "contoso/Project/_git/BigMono");
    }
}
