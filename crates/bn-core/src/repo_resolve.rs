//! Pure GitHub-name ↔ local-name repo resolution (#452).
//!
//! `bn dispatch`/`bn orchestrate --repo <name>` historically took only the *local* bare-repo name
//! (the `<name>.git` stem). But the name a human reaches for is the repo's **GitHub name**, which
//! need not match the local clone dir — e.g. GH `wira360` is cloned locally as `tracker.git`, so
//! `--repo wira360` found nothing (and, worse, broke `--json` by emitting empty output). This
//! module maps *either* form — the local bare name, a bare GitHub/ADO repo name, or a full
//! `owner/repo` slug — onto the local bare name, so `--repo wira360` and `--repo tracker` both land
//! on `tracker`.
//!
//! It is **pure** over a caller-supplied list of [`RepoIdentity`]: the CLI reads the git remotes
//! and performs the dispatch, while the *policy* — the matching order, case/`.git` folding, and
//! ambiguity handling — lives here and is unit-tested without touching git or the filesystem.

/// A locally-cloned bare repo's identity, for name resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoIdentity {
    /// The local bare `<name>.git` stem, e.g. `tracker`. Returned verbatim (canonical casing) on a
    /// successful resolve.
    pub local_name: String,
    /// The origin slug — `owner/repo` (GitHub) or `org/project/repo` (ADO) — when the remote is
    /// parseable, else `None` (no origin / an unrecognized host).
    pub remote_slug: Option<String>,
}

impl RepoIdentity {
    /// A repo with a parseable origin slug (`owner/repo` or `org/project/repo`).
    pub fn new(local_name: impl Into<String>, remote_slug: impl Into<String>) -> Self {
        RepoIdentity {
            local_name: local_name.into(),
            remote_slug: Some(remote_slug.into()),
        }
    }

    /// A repo with no usable origin (no remote / unrecognized host) — reachable only by its local
    /// name.
    pub fn local_only(local_name: impl Into<String>) -> Self {
        RepoIdentity {
            local_name: local_name.into(),
            remote_slug: None,
        }
    }

    /// The hosted repo name — the last `/`-segment of the slug (`wira360` from `eduuh/wira360`,
    /// `repo` from `org/project/repo`), folded for comparison. `None` without a remote slug.
    fn remote_repo_fold(&self) -> Option<String> {
        self.remote_slug
            .as_deref()
            .and_then(|s| s.rsplit('/').next())
            .map(fold)
    }

    /// Human display for the error listing: `tracker`, or `tracker (eduuh/wira360)` when the
    /// GitHub name differs from the local one.
    fn display(&self) -> String {
        match &self.remote_slug {
            Some(slug) => format!("{} ({slug})", self.local_name),
            None => self.local_name.clone(),
        }
    }
}

/// Why a `--repo <name>` did not resolve to exactly one local bare repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoResolveError {
    /// Nothing matched by local name, full slug, or GitHub repo name. `known` lists the candidates
    /// (with their GitHub slug where it differs) so a typo is self-correcting.
    Unknown {
        requested: String,
        known: Vec<String>,
    },
    /// A bare GitHub repo name matched more than one local clone (two owners, same repo name). The
    /// caller must disambiguate with the local name or the full `owner/repo` slug.
    Ambiguous {
        requested: String,
        matches: Vec<String>,
    },
}

impl RepoResolveError {
    /// The user-facing message the CLI renders (text) or wraps in the `--json` error envelope.
    pub fn message(&self) -> String {
        match self {
            RepoResolveError::Unknown { requested, known } => {
                if known.is_empty() {
                    format!("unknown repo '{requested}' — no repos known")
                } else {
                    format!("unknown repo '{requested}' — known: {}", known.join(", "))
                }
            }
            RepoResolveError::Ambiguous { requested, matches } => format!(
                "repo '{requested}' is ambiguous — its GitHub name matches {}; \
                 pass the local name or the full owner/repo slug",
                matches.join(", ")
            ),
        }
    }
}

/// Fold a name for case- and `.git`-insensitive comparison: trim, drop a trailing `.git`,
/// lowercase.
fn fold(s: &str) -> String {
    let s = s.trim();
    s.strip_suffix(".git").unwrap_or(s).to_lowercase()
}

/// Resolve `requested` — a local bare name, a full `owner/repo` slug, or a bare GitHub/ADO repo
/// name — to a canonical local bare name among `repos`.
///
/// Matching is case-insensitive and tolerates a trailing `.git`. Precedence, first match wins:
///   1. exact **local name** — the historical behavior, checked first so a real local repo is
///      never shadowed by another clone's GitHub name;
///   2. exact full **`owner/repo` slug**;
///   3. bare **GitHub repo name** (the slug's last segment) — [`RepoResolveError::Ambiguous`] when
///      it hits more than one clone.
///
/// An empty/whitespace `requested`, or no match at all, yields [`RepoResolveError::Unknown`].
pub fn resolve_repo_name(
    requested: &str,
    repos: &[RepoIdentity],
) -> Result<String, RepoResolveError> {
    let want = fold(requested);
    let unknown = || RepoResolveError::Unknown {
        requested: requested.trim().to_string(),
        known: repos.iter().map(RepoIdentity::display).collect(),
    };
    if want.is_empty() {
        return Err(unknown());
    }
    // (1) exact local name.
    if let Some(r) = repos.iter().find(|r| fold(&r.local_name) == want) {
        return Ok(r.local_name.clone());
    }
    // (2) exact full slug (owner/repo).
    if let Some(r) = repos
        .iter()
        .find(|r| r.remote_slug.as_deref().map(fold).as_deref() == Some(want.as_str()))
    {
        return Ok(r.local_name.clone());
    }
    // (3) bare GitHub repo name (the slug's last segment).
    let by_repo: Vec<&RepoIdentity> = repos
        .iter()
        .filter(|r| r.remote_repo_fold().as_deref() == Some(want.as_str()))
        .collect();
    match by_repo.as_slice() {
        [only] => Ok(only.local_name.clone()),
        [] => Err(unknown()),
        many => Err(RepoResolveError::Ambiguous {
            requested: requested.trim().to_string(),
            matches: many.iter().map(|r| r.local_name.clone()).collect(),
        }),
    }
}
